//! User daemon lifecycle, device supervision, and serialized stream ownership.

#[cfg(all(feature = "daemon", feature = "gstreamer"))]
mod runtime {
    use std::{
        collections::BTreeMap,
        fs, io,
        os::unix::{
            fs::PermissionsExt,
            net::{UnixListener, UnixStream},
        },
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
        DecoderPreference, RecordContainer, SharedCrop, SharedFit, SharedOutput, SharedPipeline,
        SharedRecording, SharedRotation, SharedSource, SnapshotEncoding, SnapshotRequest,
    };
    use serde_json::{Value, json};

    /// Runtime options for one daemon process.
    #[derive(Clone, Debug)]
    pub struct DaemonOptions {
        pub socket: PathBuf,
        pub device: Option<String>,
        pub decoder: DecoderPreference,
        pub decoder_device: Option<PathBuf>,
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
        let stopping = Arc::new(AtomicBool::new(false));
        let signal_stopping = Arc::clone(&stopping);
        let signal_socket = options.socket.clone();
        ctrlc::set_handler(move || {
            signal_stopping.store(true, Ordering::SeqCst);
            let _ = UnixStream::connect(&signal_socket);
        })
        .map_err(|error| {
            LinkError::new(
                ErrorKind::IoFailure,
                "failed to install daemon signal handler",
            )
            .with_detail("reason", error.to_string())
        })?;
        let (commands, actor) = start_actor(
            options.device.clone(),
            options.decoder,
            options.decoder_device.clone(),
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
                Err(error) => {
                    stopping.store(true, Ordering::SeqCst);
                    drop(commands);
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
        decoder: DecoderPreference,
        decoder_device: Option<PathBuf>,
        timeout: Duration,
        stopping: Arc<AtomicBool>,
    ) -> (
        SyncSender<ActorRequest>,
        thread::JoinHandle<Result<(), LinkError>>,
    ) {
        let (sender, receiver) = mpsc::sync_channel(32);
        let actor = thread::spawn(move || {
            actor_loop(
                receiver,
                selector,
                decoder,
                decoder_device,
                timeout,
                stopping,
            )
        });
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
        decoder: DecoderPreference,
        decoder_device: Option<PathBuf>,
        timeout: Duration,
        source: Option<SharedSource>,
        pipeline: Option<SharedPipeline>,
        outputs: BTreeMap<String, VirtualCameraSpec>,
        recording: Option<link_ipc::RecordingSpec>,
        recording_root: Option<PathBuf>,
        recording_segment: u64,
        started_unix_ms: u128,
        reloads: u64,
        reconnects: u64,
        last_error: Option<String>,
        next_retry: Instant,
        retry_delay: Duration,
    }

    impl ActorState {
        fn new(
            selector: Option<String>,
            decoder: DecoderPreference,
            decoder_device: Option<PathBuf>,
            timeout: Duration,
        ) -> Self {
            Self {
                selector,
                decoder,
                decoder_device,
                timeout,
                source: None,
                pipeline: None,
                outputs: BTreeMap::new(),
                recording: None,
                recording_root: None,
                recording_segment: 0,
                started_unix_ms: unix_ms().unwrap_or_default(),
                reloads: 0,
                reconnects: 0,
                last_error: None,
                next_retry: Instant::now(),
                retry_delay: Duration::from_millis(250),
            }
        }

        fn has_consumers(&self) -> bool {
            !self.outputs.is_empty() || self.recording.is_some()
        }

        fn ensure_source(&mut self) -> Result<&SharedSource, LinkError> {
            if self
                .source
                .as_ref()
                .is_none_or(|source| !source.node.exists())
            {
                self.source = Some(resolve_source(self.selector.as_deref())?);
            }
            Ok(self.source.as_ref().expect("source was populated"))
        }

        fn rebuild(&mut self, recovery: bool) -> Result<(), LinkError> {
            let continuing_recording = self.recording.is_some()
                && self
                    .pipeline
                    .as_ref()
                    .is_some_and(|pipeline| pipeline.graph().recording.is_some());
            if let Some(previous) = self.pipeline.take()
                && previous.graph().recording.is_some()
            {
                previous.shutdown(self.timeout.min(Duration::from_secs(2)));
            }
            if !self.has_consumers() {
                self.last_error = None;
                self.retry_delay = Duration::from_millis(250);
                return Ok(());
            }
            let source = source_for_rebuild(
                resolve_source(self.selector.as_deref())?,
                self.source.as_ref(),
                recovery,
            );
            if recovery || continuing_recording {
                self.advance_recording_segment()?;
            }
            let outputs = self
                .outputs
                .values()
                .map(shared_output)
                .collect::<Result<Vec<_>, _>>()?;
            let recording = self.recording.as_ref().map(shared_recording);
            match SharedPipeline::start(
                source.clone(),
                outputs,
                recording,
                self.decoder,
                self.decoder_device.as_deref(),
                self.timeout,
            ) {
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

        fn advance_recording_segment(&mut self) -> Result<(), LinkError> {
            let Some(recording) = self.recording.as_mut() else {
                return Ok(());
            };
            if !recording.output.exists() {
                return Ok(());
            }
            let root = self.recording_root.as_ref().unwrap_or(&recording.output);
            let (output, segment) =
                next_recovery_recording_path(root, self.recording_segment.saturating_add(1))?;
            recording.output = output;
            recording.overwrite = false;
            self.recording_segment = segment;
            Ok(())
        }

        fn poll(&mut self) {
            if let Some(error) = self.pipeline.as_ref().and_then(SharedPipeline::poll_error) {
                tracing::warn!(reason = %error, "shared pipeline failed; waiting for recovery");
                self.last_error = Some(error);
                self.pipeline.take();
                self.next_retry = Instant::now();
            }
            if self.pipeline.is_none()
                && self.has_consumers()
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
                "source_revision": link_core::source_revision(),
                "protocol_version": link_ipc::PROTOCOL_VERSION,
                "pid": std::process::id(),
                "started_unix_ms": self.started_unix_ms,
                "state": if self.pipeline.is_some() {
                    "running"
                } else if self.has_consumers() {
                    "recovering"
                } else {
                    "idle"
                },
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
        decoder: DecoderPreference,
        decoder_device: Option<PathBuf>,
        timeout: Duration,
        stopping: Arc<AtomicBool>,
    ) -> Result<(), LinkError> {
        let mut state = ActorState::new(selector, decoder, decoder_device, timeout);
        while !stopping.load(Ordering::SeqCst) {
            let request = if state.has_consumers() {
                match receiver.recv_timeout(Duration::from_millis(250)) {
                    Ok(request) => request,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        state.poll();
                        continue;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            } else {
                match receiver.recv() {
                    Ok(request) => request,
                    Err(mpsc::RecvError) => break,
                }
            };
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
                if state.has_consumers() {
                    state.rebuild(false)?;
                } else {
                    state.source = None;
                    state.last_error = None;
                }
                state.reloads = state.reloads.saturating_add(1);
                Ok((state.status(), Vec::new()))
            }
            Operation::Shutdown => {
                if let Some(pipeline) = state.pipeline.take() {
                    pipeline.shutdown(state.timeout);
                }
                state.outputs.clear();
                state.recording = None;
                state.recording_root = None;
                state.recording_segment = 0;
                stopping.store(true, Ordering::SeqCst);
                Ok((json!({"state": "stopping"}), Vec::new()))
            }
            Operation::PipelineStatus => {
                if state.source.is_none()
                    && let Err(error) = state.ensure_source()
                {
                    state.last_error = Some(error.to_string());
                }
                Ok((pipeline_status(state), Vec::new()))
            }
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
                let output = shared_output(&specification)?;
                if let Some(pipeline) = state.pipeline.as_mut() {
                    pipeline.add_output(output)?;
                    state.outputs.insert(name.clone(), specification);
                } else {
                    state.outputs.insert(name.clone(), specification);
                    if let Err(error) = state.rebuild(false) {
                        state.outputs.remove(&name);
                        let _ = state.rebuild(false);
                        return Err(error);
                    }
                }
                Ok((json!({"name": name, "state": "running"}), Vec::new()))
            }
            Operation::VcamStop { name } => {
                let names = if let Some(name) = name {
                    if !state.outputs.contains_key(&name) {
                        return Err(LinkError::new(
                            ErrorKind::DeviceNotFound,
                            "virtual camera is not active",
                        )
                        .with_detail("name", name));
                    }
                    vec![name]
                } else {
                    state.outputs.keys().cloned().collect()
                };
                if let Some(pipeline) = state.pipeline.as_mut() {
                    for name in &names {
                        pipeline.remove_output(name)?;
                        state.outputs.remove(name);
                    }
                    if !state.has_consumers() {
                        state.rebuild(false)?;
                    }
                } else {
                    for name in &names {
                        state.outputs.remove(name);
                    }
                    state.rebuild(false)?;
                }
                Ok((
                    json!({"state": "stopped", "remaining": state.outputs.len()}),
                    Vec::new(),
                ))
            }
            Operation::Snapshot { encoding } => {
                let encoding = match encoding {
                    IpcSnapshotEncoding::Jpeg => SnapshotEncoding::Jpeg,
                    IpcSnapshotEncoding::Png => SnapshotEncoding::Png,
                };
                let frame = if let Some(pipeline) = state.pipeline.as_ref() {
                    pipeline.snapshot(encoding, state.timeout)?
                } else {
                    let source = state.ensure_source()?.clone();
                    let _lease =
                        link_media::MediaLease::acquire(&source.stable_id, "linkd-snapshot")?;
                    link_media::snapshot(&SnapshotRequest {
                        node: source.node,
                        tuple: source.tuple,
                        encoding,
                        count: 1,
                        interval: Duration::ZERO,
                        timeout: state.timeout,
                    })?
                    .into_iter()
                    .next()
                    .ok_or_else(|| {
                        LinkError::new(
                            ErrorKind::MediaPipelineFailure,
                            "snapshot pipeline returned no frame",
                        )
                    })?
                };
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
                let recording = shared_recording(&specification);
                if let Some(pipeline) = state.pipeline.as_mut() {
                    pipeline.add_recording(recording)?;
                    state.recording_root = Some(specification.output.clone());
                    state.recording_segment = 0;
                    state.recording = Some(specification);
                } else {
                    state.recording_root = Some(specification.output.clone());
                    state.recording_segment = 0;
                    state.recording = Some(specification);
                    if let Err(error) = state.rebuild(false) {
                        state.recording = None;
                        state.recording_root = None;
                        state.recording_segment = 0;
                        let _ = state.rebuild(false);
                        return Err(error);
                    }
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
                if state.recording.is_none() {
                    return Err(LinkError::new(
                        ErrorKind::InvalidInvocation,
                        "no daemon recording is active",
                    ));
                }
                let finalized = if let Some(pipeline) = state.pipeline.as_mut() {
                    pipeline.remove_recording(state.timeout)?
                } else {
                    false
                };
                state.recording = None;
                state.recording_root = None;
                state.recording_segment = 0;
                if state.pipeline.is_none() || !state.has_consumers() {
                    state.rebuild(false)?;
                }
                Ok((
                    json!({"state": "stopped", "finalized": finalized}),
                    Vec::new(),
                ))
            }
        }
    }

    fn pipeline_status(state: &ActorState) -> Value {
        json!({
            "state": if state.pipeline.is_some() {
                "playing"
            } else if state.has_consumers() {
                "recovering"
            } else {
                "idle"
            },
            "source": state.source,
            "outputs": state.outputs.keys().collect::<Vec<_>>(),
            "recording": state.recording,
            "last_error": state.last_error,
        })
    }

    fn source_node(state: &mut ActorState) -> Result<&Path, LinkError> {
        Ok(state.ensure_source()?.node.as_path())
    }

    #[derive(Clone)]
    struct PreparedControl {
        descriptor: ControlDescriptor,
        value: ControlValue,
        prerequisite: bool,
    }

    fn apply_standard_controls(
        state: &mut ActorState,
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

    fn source_for_rebuild(
        mut discovered: SharedSource,
        previous: Option<&SharedSource>,
        recovery: bool,
    ) -> SharedSource {
        if recovery
            && let Some(previous) = previous
            && previous.stable_id == discovered.stable_id
        {
            discovered.tuple = previous.tuple.clone();
        }
        discovered
    }

    fn next_recovery_recording_path(
        root: &Path,
        first_segment: u64,
    ) -> Result<(PathBuf, u64), LinkError> {
        let mut segment = first_segment.max(1);
        loop {
            let candidate = recovery_recording_path(root, segment)?;
            if !candidate.exists() {
                return Ok((candidate, segment));
            }
            segment = segment.checked_add(1).ok_or_else(|| {
                LinkError::new(
                    ErrorKind::IoFailure,
                    "recording recovery segment number overflowed",
                )
                .with_detail("path", root.display().to_string())
            })?;
        }
    }

    fn recovery_recording_path(root: &Path, segment: u64) -> Result<PathBuf, LinkError> {
        let stem = root.file_stem().ok_or_else(|| {
            LinkError::new(
                ErrorKind::InvalidInvocation,
                "recording output has no valid file stem",
            )
            .with_detail("path", root.display().to_string())
        })?;
        let parent = root.parent().unwrap_or_else(|| Path::new("."));
        let mut name = stem.to_os_string();
        name.push(format!(".reconnect-{segment:03}"));
        if let Some(extension) = root.extension() {
            name.push(".");
            name.push(extension);
        }
        Ok(parent.join(name))
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
        use link_core::{media::VideoTuple, probe::Rational};
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

        #[test]
        fn recovery_preserves_the_active_source_contract() {
            let active = SharedSource {
                stable_id: "camera-a".into(),
                node: PathBuf::from("/dev/video0"),
                tuple: VideoTuple {
                    fourcc: "H264".into(),
                    width: 1920,
                    height: 1080,
                    fps: Rational {
                        numerator: 60,
                        denominator: 1,
                    },
                },
            };
            let discovered = SharedSource {
                stable_id: active.stable_id.clone(),
                node: PathBuf::from("/dev/video2"),
                tuple: VideoTuple {
                    fourcc: "MJPG".into(),
                    width: 1920,
                    height: 1080,
                    fps: Rational {
                        numerator: 30,
                        denominator: 1,
                    },
                },
            };
            let recovered = source_for_rebuild(discovered, Some(&active), true);
            assert_eq!(recovered.node, PathBuf::from("/dev/video2"));
            assert_eq!(recovered.tuple, active.tuple);
        }

        #[test]
        fn recording_recovery_uses_a_deterministic_sibling() {
            let recovered = recovery_recording_path(Path::new("/tmp/meeting.mkv"), 2).unwrap();
            assert_eq!(recovered, PathBuf::from("/tmp/meeting.reconnect-002.mkv"));
        }

        #[test]
        fn daemon_stays_idle_until_a_persistent_consumer_exists() {
            let mut state = ActorState::new(
                None,
                DecoderPreference::Software,
                None,
                Duration::from_secs(1),
            );
            assert!(!state.has_consumers());
            assert_eq!(state.status()["state"], "idle");

            state
                .outputs
                .insert("clean".into(), VirtualCameraSpec::default());
            assert!(state.has_consumers());
            assert_eq!(state.status()["state"], "recovering");
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
