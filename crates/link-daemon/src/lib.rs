//! User daemon lifecycle, device supervision, and serialized stream ownership.

#[cfg(all(feature = "daemon", feature = "gstreamer"))]
mod runtime {
    use std::{
        collections::BTreeMap,
        fs, io,
        os::unix::{fs::PermissionsExt, net::UnixListener},
        path::{Path, PathBuf},
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
            mpsc::{self, Receiver, SyncSender},
        },
        thread,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    use link_core::{
        ErrorKind, LinkError,
        control::{
            ControlChangeReport, ControlDescriptor, ControlSetReport, ControlValue, RollbackReport,
        },
        probe::VideoNodeKind,
    };
    use link_ipc::{
        Operation, RecordingContainer, RequestEnvelope, ResponseEnvelope, Rotation,
        SnapshotEncoding as IpcSnapshotEncoding, StandardControlWrite, VirtualCameraSpec,
    };
    use link_media::{
        RecordContainer, SharedCrop, SharedFit, SharedOutput, SharedPipeline, SharedRecording,
        SharedRotation, SharedSource, SnapshotEncoding,
    };
    use serde_json::{Value, json};

    /// Runtime options for one daemon process.
    #[derive(Clone, Debug)]
    pub struct DaemonOptions {
        pub socket: PathBuf,
        pub device: Option<String>,
        pub request_timeout: Duration,
    }

    /// Bind the private socket, supervise the camera, and serve until graceful shutdown.
    pub fn run(options: DaemonOptions) -> Result<(), LinkError> {
        let parent = options.socket.parent().ok_or_else(|| {
            LinkError::new(
                ErrorKind::InvalidInvocation,
                "daemon socket has no parent directory",
            )
        })?;
        link_core::paths::AppPaths::ensure_private(parent)?;
        if options.socket.exists() {
            if std::os::unix::net::UnixStream::connect(&options.socket).is_ok() {
                return Err(LinkError::new(
                    ErrorKind::DeviceBusy,
                    "another linkd process is already serving this socket",
                )
                .with_detail("socket", options.socket.display().to_string()));
            }
            link_ipc::remove_stale_socket(&options.socket)?;
        }
        let listener = UnixListener::bind(&options.socket).map_err(|error| {
            socket_error("failed to bind daemon socket", &options.socket, &error)
        })?;
        fs::set_permissions(&options.socket, fs::Permissions::from_mode(0o600)).map_err(
            |error| socket_error("failed to secure daemon socket", &options.socket, &error),
        )?;
        listener.set_nonblocking(true).map_err(|error| {
            socket_error("failed to configure daemon socket", &options.socket, &error)
        })?;

        let stopping = Arc::new(AtomicBool::new(false));
        let signal_stopping = Arc::clone(&stopping);
        ctrlc::set_handler(move || signal_stopping.store(true, Ordering::SeqCst)).map_err(
            |error| {
                LinkError::new(
                    ErrorKind::IoFailure,
                    "failed to install daemon signal handler",
                )
                .with_detail("reason", error.to_string())
            },
        )?;
        let (commands, actor) = start_actor(
            options.device.clone(),
            options.request_timeout,
            Arc::clone(&stopping),
        );
        tracing::info!(socket = %options.socket.display(), "linkd is ready");
        while !stopping.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let result = handle_connection(&mut stream, &commands, options.request_timeout);
                    if let Err(error) = result {
                        tracing::warn!(
                            kind = error.kind().code(),
                            message = error.message(),
                            "IPC request failed"
                        );
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(25));
                }
                Err(error) => {
                    stopping.store(true, Ordering::SeqCst);
                    let _ = actor.join();
                    let _ = fs::remove_file(&options.socket);
                    return Err(socket_error(
                        "failed to accept daemon connection",
                        &options.socket,
                        &error,
                    ));
                }
            }
        }
        drop(commands);
        actor
            .join()
            .map_err(|_| LinkError::new(ErrorKind::IoFailure, "daemon actor thread panicked"))??;
        let _ = fs::remove_file(&options.socket);
        tracing::info!("linkd stopped");
        Ok(())
    }

    struct ActorRequest {
        envelope: RequestEnvelope,
        response: mpsc::Sender<(ResponseEnvelope, Vec<u8>)>,
    }

    fn start_actor(
        selector: Option<String>,
        timeout: Duration,
        stopping: Arc<AtomicBool>,
    ) -> (
        SyncSender<ActorRequest>,
        thread::JoinHandle<Result<(), LinkError>>,
    ) {
        let (sender, receiver) = mpsc::sync_channel(32);
        let actor = thread::spawn(move || actor_loop(receiver, selector, timeout, stopping));
        (sender, actor)
    }

    fn handle_connection(
        stream: &mut std::os::unix::net::UnixStream,
        commands: &SyncSender<ActorRequest>,
        timeout: Duration,
    ) -> Result<(), LinkError> {
        stream
            .set_read_timeout(Some(timeout))
            .map_err(transport_error)?;
        stream
            .set_write_timeout(Some(timeout))
            .map_err(transport_error)?;
        link_ipc::verify_peer_uid(stream)?;
        let (request, binary): (RequestEnvelope, Vec<u8>) = link_ipc::read_message(stream)?;
        if !binary.is_empty() {
            return Err(LinkError::new(
                ErrorKind::InvalidInvocation,
                "daemon request did not expect a binary body",
            ));
        }
        if let Err(error) = link_ipc::validate_protocol(request.protocol_version) {
            let response = error_response(request.request_id, &error);
            return link_ipc::write_message(stream, &response, &[]);
        }
        let (response_sender, response_receiver) = mpsc::channel();
        commands
            .try_send(ActorRequest {
                envelope: request,
                response: response_sender,
            })
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => {
                    LinkError::new(ErrorKind::DeviceBusy, "daemon actor request queue is full")
                }
                mpsc::TrySendError::Disconnected(_) => daemon_stopped(),
            })?;
        let actor_timeout = timeout.saturating_add(Duration::from_secs(1));
        let (response, binary) =
            response_receiver
                .recv_timeout(actor_timeout)
                .map_err(|error| match error {
                    mpsc::RecvTimeoutError::Timeout => {
                        LinkError::new(ErrorKind::Timeout, "timed out waiting for the daemon actor")
                    }
                    mpsc::RecvTimeoutError::Disconnected => daemon_stopped(),
                })?;
        link_ipc::write_message(stream, &response, &binary)
    }

    struct ActorState {
        selector: Option<String>,
        timeout: Duration,
        source: Option<SharedSource>,
        pipeline: Option<SharedPipeline>,
        outputs: BTreeMap<String, VirtualCameraSpec>,
        recording: Option<link_ipc::RecordingSpec>,
        started_unix_ms: u128,
        reloads: u64,
        reconnects: u64,
        last_error: Option<String>,
        next_retry: Instant,
        retry_delay: Duration,
    }

    impl ActorState {
        fn new(selector: Option<String>, timeout: Duration) -> Self {
            Self {
                selector,
                timeout,
                source: None,
                pipeline: None,
                outputs: BTreeMap::new(),
                recording: None,
                started_unix_ms: unix_ms().unwrap_or_default(),
                reloads: 0,
                reconnects: 0,
                last_error: None,
                next_retry: Instant::now(),
                retry_delay: Duration::from_millis(250),
            }
        }

        fn rebuild(&mut self, recovery: bool) -> Result<(), LinkError> {
            if let Some(previous) = self.pipeline.take()
                && previous.graph().recording.is_some()
            {
                previous.shutdown(self.timeout.min(Duration::from_secs(2)));
            }
            let source = resolve_source(self.selector.as_deref())?;
            let outputs = self
                .outputs
                .values()
                .map(shared_output)
                .collect::<Result<Vec<_>, _>>()?;
            let recording = self.recording.as_ref().map(shared_recording);
            match SharedPipeline::start(source.clone(), outputs, recording, self.timeout) {
                Ok(pipeline) => {
                    self.source = Some(source);
                    self.pipeline = Some(pipeline);
                    self.last_error = None;
                    self.retry_delay = Duration::from_millis(250);
                    if recovery {
                        self.reconnects = self.reconnects.saturating_add(1);
                    }
                    Ok(())
                }
                Err(error) => {
                    self.source = Some(source);
                    self.last_error = Some(error.to_string());
                    Err(error)
                }
            }
        }

        fn poll(&mut self) {
            if let Some(error) = self.pipeline.as_ref().and_then(SharedPipeline::poll_error) {
                tracing::warn!(reason = %error, "shared pipeline failed; waiting for recovery");
                self.last_error = Some(error);
                self.pipeline.take();
                self.next_retry = Instant::now();
            }
            if self.pipeline.is_none()
                && Instant::now() >= self.next_retry
                && let Err(error) = self.rebuild(true)
            {
                self.last_error = Some(error.to_string());
                self.next_retry = Instant::now() + self.retry_delay;
                self.retry_delay = (self.retry_delay * 2).min(Duration::from_secs(5));
            }
        }

        fn status(&self) -> Value {
            json!({
                "version": env!("CARGO_PKG_VERSION"),
                "protocol_version": link_ipc::PROTOCOL_VERSION,
                "pid": std::process::id(),
                "started_unix_ms": self.started_unix_ms,
                "state": if self.pipeline.is_some() { "running" } else { "recovering" },
                "source": self.source,
                "virtual_cameras": self.outputs.len(),
                "recording": self.recording,
                "reloads": self.reloads,
                "reconnects": self.reconnects,
                "last_error": self.last_error,
            })
        }
    }

    fn actor_loop(
        receiver: Receiver<ActorRequest>,
        selector: Option<String>,
        timeout: Duration,
        stopping: Arc<AtomicBool>,
    ) -> Result<(), LinkError> {
        let mut state = ActorState::new(selector, timeout);
        if let Err(error) = state.rebuild(false) {
            tracing::warn!(
                kind = error.kind().code(),
                message = error.message(),
                "camera unavailable at daemon startup"
            );
        }
        while !stopping.load(Ordering::SeqCst) {
            match receiver.recv_timeout(Duration::from_millis(100)) {
                Ok(request) => {
                    let request_id = request.envelope.request_id;
                    let result = dispatch(&mut state, request.envelope.operation, &stopping);
                    let (response, binary) = match result {
                        Ok((value, binary)) => (
                            ResponseEnvelope {
                                protocol_version: link_ipc::PROTOCOL_VERSION,
                                request_id,
                                result: Ok(value),
                                binary_length: binary.len() as u64,
                            },
                            binary,
                        ),
                        Err(error) => (error_response(request_id, &error), Vec::new()),
                    };
                    let _ = request.response.send((response, binary));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => state.poll(),
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        if let Some(pipeline) = state.pipeline.take() {
            pipeline.shutdown(timeout);
        }
        Ok(())
    }

    fn dispatch(
        state: &mut ActorState,
        operation: Operation,
        stopping: &AtomicBool,
    ) -> Result<(Value, Vec<u8>), LinkError> {
        match operation {
            Operation::Status => Ok((state.status(), Vec::new())),
            Operation::Reload => {
                state.rebuild(false)?;
                state.reloads = state.reloads.saturating_add(1);
                Ok((state.status(), Vec::new()))
            }
            Operation::Shutdown => {
                if let Some(pipeline) = state.pipeline.take() {
                    pipeline.shutdown(state.timeout);
                }
                state.outputs.clear();
                state.recording = None;
                stopping.store(true, Ordering::SeqCst);
                Ok((json!({"state": "stopping"}), Vec::new()))
            }
            Operation::PipelineStatus => Ok((pipeline_status(state), Vec::new())),
            Operation::PipelineGraph => {
                let graph = state.pipeline.as_ref().map(SharedPipeline::graph);
                Ok((
                    serde_json::to_value(graph).unwrap_or(Value::Null),
                    Vec::new(),
                ))
            }
            Operation::PipelineMetrics => {
                let metrics = state
                    .pipeline
                    .as_ref()
                    .map(|pipeline| pipeline.metrics(state.reconnects, state.last_error.clone()));
                Ok((
                    serde_json::to_value(metrics).unwrap_or(Value::Null),
                    Vec::new(),
                ))
            }
            Operation::ControlList => {
                let controls =
                    link_v4l2::production::ControlDevice::open_read(source_node(state)?)?
                        .controls()?;
                Ok((
                    serde_json::to_value(controls).unwrap_or_default(),
                    Vec::new(),
                ))
            }
            Operation::ControlGet { selector } => {
                let device = link_v4l2::production::ControlDevice::open_read(source_node(state)?)?;
                let descriptor = device.resolve(&selector)?;
                let (control, value) = device.get(descriptor.id)?;
                Ok((json!({"control": control, "value": value}), Vec::new()))
            }
            Operation::ControlSet {
                writes,
                raw,
                clamp,
                batched,
                fallback_individual,
                dry_run,
            } => Ok((
                serde_json::to_value(apply_standard_controls(
                    state,
                    writes,
                    raw,
                    clamp,
                    batched,
                    fallback_individual,
                    dry_run,
                )?)
                .unwrap_or_default(),
                Vec::new(),
            )),
            Operation::ControlReset {
                selector,
                raw,
                dry_run,
            } => {
                let device = link_v4l2::production::ControlDevice::open_read(source_node(state)?)?;
                let descriptor = device.resolve(&selector)?;
                if !descriptor.default_is_valid {
                    return Err(LinkError::new(
                        ErrorKind::CapabilityUnsupported,
                        "driver-advertised default is invalid and will not be written",
                    )
                    .with_detail("control", descriptor.name)
                    .with_detail("default", descriptor.default)
                    .with_detail("minimum", descriptor.minimum)
                    .with_detail("maximum", descriptor.maximum));
                }
                Ok((
                    serde_json::to_value(apply_standard_controls(
                        state,
                        vec![StandardControlWrite {
                            selector: descriptor.id.to_string(),
                            value: descriptor.default.to_string(),
                        }],
                        raw,
                        false,
                        false,
                        false,
                        dry_run,
                    )?)
                    .unwrap_or_default(),
                    Vec::new(),
                ))
            }
            Operation::VcamList => Ok((
                serde_json::to_value(state.outputs.values().collect::<Vec<_>>())
                    .unwrap_or_default(),
                Vec::new(),
            )),
            Operation::VcamStatus { name } => {
                let outputs: Vec<_> = state
                    .outputs
                    .values()
                    .filter(|output| name.as_ref().is_none_or(|name| &output.name == name))
                    .collect();
                if name.is_some() && outputs.is_empty() {
                    return Err(LinkError::new(
                        ErrorKind::DeviceNotFound,
                        "virtual camera is not active",
                    ));
                }
                Ok((
                    json!({"outputs": outputs, "pipeline": pipeline_status(state)}),
                    Vec::new(),
                ))
            }
            Operation::VcamStart { specification } => {
                if state.outputs.contains_key(&specification.name) {
                    return Err(LinkError::new(
                        ErrorKind::InvalidInvocation,
                        "virtual-camera name is already active",
                    )
                    .with_detail("name", specification.name));
                }
                validate_output_device(&specification.output_device)?;
                let name = specification.name.clone();
                state.outputs.insert(name.clone(), specification);
                if let Err(error) = state.rebuild(false) {
                    state.outputs.remove(&name);
                    let _ = state.rebuild(false);
                    return Err(error);
                }
                Ok((json!({"name": name, "state": "running"}), Vec::new()))
            }
            Operation::VcamStop { name } => {
                if let Some(name) = name {
                    if state.outputs.remove(&name).is_none() {
                        return Err(LinkError::new(
                            ErrorKind::DeviceNotFound,
                            "virtual camera is not active",
                        )
                        .with_detail("name", name));
                    }
                } else {
                    state.outputs.clear();
                }
                state.rebuild(false)?;
                Ok((
                    json!({"state": "stopped", "remaining": state.outputs.len()}),
                    Vec::new(),
                ))
            }
            Operation::Snapshot { encoding } => {
                let pipeline = state
                    .pipeline
                    .as_ref()
                    .ok_or_else(|| pipeline_unavailable(state))?;
                let encoding = match encoding {
                    IpcSnapshotEncoding::Jpeg => SnapshotEncoding::Jpeg,
                    IpcSnapshotEncoding::Png => SnapshotEncoding::Png,
                };
                let frame = pipeline.snapshot(encoding, state.timeout)?;
                Ok((
                    json!({"captured_unix_ms": frame.captured_unix_ms, "bytes": frame.bytes.len()}),
                    frame.bytes,
                ))
            }
            Operation::RecordingStart { specification } => {
                if state.recording.is_some() {
                    return Err(LinkError::new(
                        ErrorKind::DeviceBusy,
                        "a daemon recording is already active",
                    ));
                }
                let original = specification.clone();
                state.recording = Some(specification);
                if let Err(error) = state.rebuild(false) {
                    state.recording = None;
                    let _ = state.rebuild(false);
                    return Err(error);
                }
                Ok((
                    json!({"state": "recording", "recording": original}),
                    Vec::new(),
                ))
            }
            Operation::RecordingStatus => Ok((
                json!({
                    "state": if state.recording.is_some() { "recording" } else { "idle" },
                    "recording": state.recording,
                }),
                Vec::new(),
            )),
            Operation::RecordingStop => {
                if state.recording.take().is_none() {
                    return Err(LinkError::new(
                        ErrorKind::InvalidInvocation,
                        "no daemon recording is active",
                    ));
                }
                state.rebuild(false)?;
                Ok((json!({"state": "stopped"}), Vec::new()))
            }
        }
    }

    fn pipeline_status(state: &ActorState) -> Value {
        json!({
            "state": if state.pipeline.is_some() { "playing" } else { "recovering" },
            "source": state.source,
            "outputs": state.outputs.keys().collect::<Vec<_>>(),
            "recording": state.recording,
            "last_error": state.last_error,
        })
    }

    fn source_node(state: &ActorState) -> Result<&Path, LinkError> {
        state
            .source
            .as_ref()
            .map(|source| source.node.as_path())
            .ok_or_else(|| pipeline_unavailable(state))
    }

    #[derive(Clone)]
    struct PreparedControl {
        descriptor: ControlDescriptor,
        value: ControlValue,
        prerequisite: bool,
    }

    fn apply_standard_controls(
        state: &ActorState,
        writes: Vec<StandardControlWrite>,
        raw: bool,
        clamp: bool,
        batched: bool,
        fallback_individual: bool,
        dry_run: bool,
    ) -> Result<ControlSetReport, LinkError> {
        if writes.is_empty() {
            return Err(LinkError::new(
                ErrorKind::InvalidInvocation,
                "control set requires at least one write",
            ));
        }
        let path = source_node(state)?;
        let reader = link_v4l2::production::ControlDevice::open_read(path)?;
        let mut prepared = Vec::new();
        let mut prerequisite_ids = Vec::new();
        for write in writes {
            let descriptor = reader.resolve(&write.selector)?;
            if !raw {
                for (parent_id, manual_value) in
                    link_v4l2::production::manual_dependencies(descriptor.id)
                {
                    if prerequisite_ids.contains(&parent_id) {
                        continue;
                    }
                    let Ok(parent) = reader.query(parent_id) else {
                        continue;
                    };
                    if parent.current != Some(manual_value) {
                        prepared.push(PreparedControl {
                            value: link_v4l2::production::render_value(&parent, manual_value),
                            descriptor: parent,
                            prerequisite: true,
                        });
                    }
                    prerequisite_ids.push(parent_id);
                }
            }
            let value = link_v4l2::production::parse_value(&descriptor, &write.value, clamp)?;
            link_v4l2::production::validate_raw_value(&descriptor, value.raw)?;
            prepared.push(PreparedControl {
                descriptor,
                value,
                prerequisite: false,
            });
        }

        let mut previous = Vec::with_capacity(prepared.len());
        for write in &prepared {
            previous.push(reader.get(write.descriptor.id).ok().map(|(_, value)| value));
        }
        let requested_count = prepared.iter().filter(|write| !write.prerequisite).count();
        if dry_run {
            return Ok(control_report(
                prepared,
                previous,
                true,
                batched || requested_count > 1,
                false,
                None,
            ));
        }

        let writer = link_v4l2::production::ControlDevice::open_write(path)?;
        for write in prepared.iter().filter(|write| write.prerequisite) {
            if let Err(error) = writer.set(&write.descriptor, write.value.raw) {
                let rollback = rollback_standard_controls(&writer, &prepared, &previous);
                return Err(control_write_error(error, None, &rollback));
            }
        }
        for write in prepared.iter_mut().filter(|write| !write.prerequisite) {
            match writer.query(write.descriptor.id) {
                Ok(descriptor) if descriptor.available => write.descriptor = descriptor,
                Ok(descriptor) => {
                    let error = LinkError::new(
                        ErrorKind::CapabilityUnsupported,
                        "V4L2 control remained unavailable after changing its prerequisite",
                    )
                    .with_detail("control", descriptor.name);
                    let rollback = rollback_standard_controls(&writer, &prepared, &previous);
                    return Err(control_write_error(error, None, &rollback));
                }
                Err(error) => {
                    let rollback = rollback_standard_controls(&writer, &prepared, &previous);
                    return Err(control_write_error(error, None, &rollback));
                }
            }
        }
        let raw_writes = prepared
            .iter()
            .filter(|write| !write.prerequisite)
            .map(|write| link_v4l2::production::RawControlWrite {
                descriptor: write.descriptor.clone(),
                value: write.value.raw,
            })
            .collect::<Vec<_>>();
        let mut fallback_used = false;
        let mut error_index = None;
        let write_result = if batched || raw_writes.len() > 1 {
            match writer.set_batch(&raw_writes) {
                Ok(()) => Ok(()),
                Err(batch_error) if fallback_individual => {
                    error_index = Some(batch_error.error_index);
                    let rollback = rollback_standard_controls(&writer, &prepared, &previous);
                    if !rollback.failed.is_empty() {
                        return Err(control_partial_error(
                            "batch failed and rollback was incomplete",
                            error_index,
                            &rollback,
                        ));
                    }
                    for write in prepared.iter().filter(|write| write.prerequisite) {
                        if let Err(error) = writer.set(&write.descriptor, write.value.raw) {
                            let rollback =
                                rollback_standard_controls(&writer, &prepared, &previous);
                            return Err(control_write_error(error, error_index, &rollback));
                        }
                    }
                    fallback_used = true;
                    raw_writes
                        .iter()
                        .try_for_each(|write| {
                            writer.set(&write.descriptor, write.value).map(|_| ())
                        })
                        .map_err(|error| (error, error_index))
                }
                Err(batch_error) => Err((batch_error.error, Some(batch_error.error_index))),
            }
        } else {
            writer
                .set(&raw_writes[0].descriptor, raw_writes[0].value)
                .map(|_| ())
                .map_err(|error| (error, None))
        };
        if let Err((error, error_index)) = write_result {
            let rollback = rollback_standard_controls(&writer, &prepared, &previous);
            return Err(control_write_error(error, error_index, &rollback));
        }

        let verifier = link_v4l2::production::ControlDevice::open_read(path)?;
        let mut changes = Vec::with_capacity(prepared.len());
        let mut verified = true;
        for (write, previous) in prepared.iter().zip(previous.iter()) {
            let observed = verifier
                .get(write.descriptor.id)
                .ok()
                .map(|(_, value)| value);
            let matches = observed
                .as_ref()
                .is_some_and(|value| value.raw == write.value.raw)
                || !write.descriptor.readable;
            verified &= matches;
            changes.push(ControlChangeReport {
                control: verifier
                    .query(write.descriptor.id)
                    .unwrap_or_else(|_| write.descriptor.clone()),
                previous: previous.clone(),
                requested: write.value.clone(),
                applied: Some(write.value.clone()),
                observed,
                verified: matches,
                prerequisite: write.prerequisite,
            });
        }
        if !verified {
            let rollback = rollback_standard_controls(&writer, &prepared, &previous);
            return Err(control_partial_error(
                "V4L2 control readback did not match the requested value",
                error_index,
                &rollback,
            ));
        }
        Ok(ControlSetReport {
            changes,
            dry_run: false,
            batched: batched || raw_writes.len() > 1,
            individual_fallback_used: fallback_used,
            error_index,
            rollback: RollbackReport::default(),
        })
    }

    fn control_report(
        prepared: Vec<PreparedControl>,
        previous: Vec<Option<ControlValue>>,
        dry_run: bool,
        batched: bool,
        individual_fallback_used: bool,
        error_index: Option<u32>,
    ) -> ControlSetReport {
        ControlSetReport {
            changes: prepared
                .into_iter()
                .zip(previous)
                .map(|(write, previous)| ControlChangeReport {
                    control: write.descriptor,
                    previous: previous.clone(),
                    requested: write.value,
                    applied: None,
                    observed: previous,
                    verified: false,
                    prerequisite: write.prerequisite,
                })
                .collect(),
            dry_run,
            batched,
            individual_fallback_used,
            error_index,
            rollback: RollbackReport::default(),
        }
    }

    fn rollback_standard_controls(
        writer: &link_v4l2::production::ControlDevice,
        prepared: &[PreparedControl],
        previous: &[Option<ControlValue>],
    ) -> RollbackReport {
        let mut report = RollbackReport {
            attempted: true,
            ..RollbackReport::default()
        };
        let prerequisite_ids = prepared
            .iter()
            .filter(|write| write.prerequisite)
            .map(|write| write.descriptor.id)
            .collect::<Vec<_>>();
        let mut restored_ids = Vec::new();
        for (write, previous) in prepared
            .iter()
            .zip(previous)
            .rev()
            .filter(|(write, _)| !prerequisite_ids.contains(&write.descriptor.id))
            .chain(
                prepared
                    .iter()
                    .zip(previous)
                    .rev()
                    .filter(|(write, _)| prerequisite_ids.contains(&write.descriptor.id)),
            )
        {
            if restored_ids.contains(&write.descriptor.id) {
                continue;
            }
            restored_ids.push(write.descriptor.id);
            let Some(previous) = previous else {
                report.failed.push(write.descriptor.name.clone());
                continue;
            };
            if writer
                .get(write.descriptor.id)
                .is_ok_and(|(_, current)| current.raw == previous.raw)
            {
                report.restored.push(write.descriptor.name.clone());
                continue;
            }
            match writer.set(&write.descriptor, previous.raw) {
                Ok(_) => report.restored.push(write.descriptor.name.clone()),
                Err(_) => report.failed.push(write.descriptor.name.clone()),
            }
        }
        report
    }

    fn control_write_error(
        error: LinkError,
        error_index: Option<u32>,
        rollback: &RollbackReport,
    ) -> LinkError {
        let kind = if rollback.failed.is_empty() {
            error.kind()
        } else {
            ErrorKind::PartialSuccess
        };
        let mut result = LinkError::new(kind, error.message()).with_detail(
            "rollback",
            serde_json::to_value(rollback).unwrap_or_default(),
        );
        if let Some(error_index) = error_index {
            result = result.with_detail("error_index", u64::from(error_index));
        }
        result
    }

    fn control_partial_error(
        message: &'static str,
        error_index: Option<u32>,
        rollback: &RollbackReport,
    ) -> LinkError {
        let mut error = LinkError::new(ErrorKind::PartialSuccess, message).with_detail(
            "rollback",
            serde_json::to_value(rollback).unwrap_or_default(),
        );
        if let Some(error_index) = error_index {
            error = error.with_detail("error_index", u64::from(error_index));
        }
        error
    }

    fn shared_output(specification: &VirtualCameraSpec) -> Result<SharedOutput, LinkError> {
        Ok(SharedOutput {
            name: specification.name.clone(),
            device: specification.output_device.clone(),
            width: specification.width,
            height: specification.height,
            fps_numerator: specification.fps_numerator,
            fps_denominator: specification.fps_denominator,
            format: specification.format.clone(),
            rotation: match specification.rotation {
                Rotation::None => SharedRotation::None,
                Rotation::Clockwise90 => SharedRotation::Clockwise90,
                Rotation::Rotate180 => SharedRotation::Rotate180,
                Rotation::Counterclockwise90 => SharedRotation::Counterclockwise90,
            },
            horizontal_flip: specification.horizontal_flip,
            vertical_flip: specification.vertical_flip,
            crop: specification.crop.map(|crop| SharedCrop {
                x: crop.x,
                y: crop.y,
                width: crop.width,
                height: crop.height,
            }),
            fit: match specification.fit {
                link_ipc::FitMode::Contain => SharedFit::Contain,
                link_ipc::FitMode::Cover => SharedFit::Cover,
                link_ipc::FitMode::Stretch => SharedFit::Stretch,
            },
            zoom: specification.zoom,
            frame_x: specification.frame_x,
            frame_y: specification.frame_y,
            text_overlay: specification.text_overlay.clone(),
            image_overlay: specification.image_overlay.clone(),
            privacy_frame: specification.privacy_frame,
        })
    }

    fn shared_recording(specification: &link_ipc::RecordingSpec) -> SharedRecording {
        SharedRecording {
            output: specification.output.clone(),
            container: match specification.container {
                RecordingContainer::Matroska => RecordContainer::Matroska,
                RecordingContainer::Mp4 => RecordContainer::Mp4,
            },
            overwrite: specification.overwrite,
        }
    }

    fn resolve_source(selector: Option<&str>) -> Result<SharedSource, LinkError> {
        let devices: Vec<_> = link_linux::enumerate_devices()?
            .into_iter()
            .filter(link_linux::is_listable)
            .collect();
        let device = if let Some(selector) = selector {
            link_linux::select_devices(&devices, selector)?
                .into_iter()
                .next()
                .cloned()
                .ok_or_else(|| {
                    LinkError::new(ErrorKind::DeviceNotFound, "no camera was selected")
                })?
        } else {
            match devices.as_slice() {
                [device] => device.clone(),
                [] => {
                    return Err(LinkError::new(
                        ErrorKind::DeviceNotFound,
                        "no camera device was discovered",
                    ));
                }
                _ => {
                    return Err(LinkError::new(
                        ErrorKind::InvalidInvocation,
                        "multiple cameras were discovered; start linkd with --device",
                    ));
                }
            }
        };
        let node = if let Some(selector) =
            selector.and_then(|value| device.selected_video_node(value))
        {
            selector.association.clone()
        } else {
            device
                .video_nodes
                .iter()
                .find(|node| {
                    link_v4l2::probe_node(node.association.clone()).kind == VideoNodeKind::Capture
                })
                .map(|node| node.association.clone())
                .ok_or_else(|| {
                    LinkError::new(
                        ErrorKind::CapabilityUnsupported,
                        "selected camera has no V4L2 capture node",
                    )
                })?
        };
        let status = link_v4l2::video::VideoDevice::open_read(&node.path)?.status()?;
        Ok(SharedSource {
            stable_id: device.identity.stable_id(),
            node: PathBuf::from(node.path),
            tuple: status.tuple,
        })
    }

    fn validate_output_device(path: &Path) -> Result<(), LinkError> {
        use std::os::unix::fs::FileTypeExt;
        let metadata = fs::metadata(path).map_err(|error| {
            LinkError::new(
                ErrorKind::DeviceNotFound,
                "virtual-camera output device does not exist",
            )
            .with_detail("path", path.display().to_string())
            .with_detail("reason", error.to_string())
        })?;
        if !metadata.file_type().is_char_device() {
            return Err(LinkError::new(
                ErrorKind::InvalidInvocation,
                "virtual-camera output must be a V4L2 character device",
            )
            .with_detail("path", path.display().to_string()));
        }
        if !link_v4l2::is_video_output(path)? {
            return Err(LinkError::new(
                ErrorKind::CapabilityUnsupported,
                "selected device is not a V4L2 video-output node",
            )
            .with_detail("path", path.display().to_string()));
        }
        Ok(())
    }

    fn pipeline_unavailable(state: &ActorState) -> LinkError {
        let mut error = LinkError::new(
            ErrorKind::DaemonUnavailable,
            "daemon camera pipeline is recovering",
        );
        if let Some(reason) = &state.last_error {
            error = error.with_detail("reason", reason.clone());
        }
        error
    }

    fn error_response(request_id: u64, error: &LinkError) -> ResponseEnvelope {
        ResponseEnvelope {
            protocol_version: link_ipc::PROTOCOL_VERSION,
            request_id,
            result: Err(error.into()),
            binary_length: 0,
        }
    }

    fn transport_error(error: io::Error) -> LinkError {
        LinkError::new(
            ErrorKind::DaemonUnavailable,
            "failed to configure IPC connection",
        )
        .with_detail("reason", error.to_string())
    }

    fn socket_error(message: &'static str, path: &Path, error: &io::Error) -> LinkError {
        LinkError::new(ErrorKind::DaemonUnavailable, message)
            .with_detail("socket", path.display().to_string())
            .with_detail("reason", error.to_string())
    }

    fn daemon_stopped() -> LinkError {
        LinkError::new(ErrorKind::DaemonUnavailable, "daemon actor is not running")
    }

    fn unix_ms() -> Result<u128, LinkError> {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .map_err(|error| {
                LinkError::new(
                    ErrorKind::IoFailure,
                    "system clock is before the Unix epoch",
                )
                .with_detail("reason", error.to_string())
            })
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use link_ipc::{FitMode, NormalizedCrop};

        #[test]
        fn output_mapping_preserves_transform_contract() {
            let specification = VirtualCameraSpec {
                name: "mirror".into(),
                output_device: PathBuf::from("/dev/video20"),
                width: 1280,
                height: 720,
                horizontal_flip: true,
                crop: Some(NormalizedCrop {
                    x: 0.1,
                    y: 0.2,
                    width: 0.8,
                    height: 0.6,
                }),
                fit: FitMode::Cover,
                ..VirtualCameraSpec::default()
            };
            let mapped = shared_output(&specification).unwrap();
            assert_eq!(mapped.name, "mirror");
            assert_eq!(mapped.width, 1280);
            assert!(mapped.horizontal_flip);
            assert_eq!(mapped.crop.unwrap().x, 0.1);
        }
    }
}

#[cfg(all(feature = "daemon", feature = "gstreamer"))]
pub use runtime::{DaemonOptions, run};

#[cfg(not(all(feature = "daemon", feature = "gstreamer")))]
pub fn unavailable() -> link_core::LinkError {
    link_core::LinkError::new(
        link_core::ErrorKind::CapabilityUnsupported,
        "this build does not include daemon and GStreamer support",
    )
}
