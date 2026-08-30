# Legal and clean-room notice

This document records the project's engineering assessment and contribution policy. It is not legal advice, does not
create an attorney-client relationship, and cannot determine legality for every contributor, user, distributor, or
country. Contract terms, patents, anti-circumvention rules, privacy law, export controls, and local exceptions vary.
Obtain qualified advice when the risk matters.

## Project identity and purpose

`linkctl` is an independent, unofficial Linux interoperability project. It is not affiliated with, sponsored by, or
endorsed by Insta360. “Insta360” and product names are used only to identify the hardware with which the software is
intended to interoperate; no logo, trade dress, or claim of official origin is used.

Insta360's [Link 2C Pro compatibility documentation](https://onlinemanual.insta360.com/link2cpro/en-us/faq/compatibility)
states that the device supports Linux through UVC/UAC and that Link Controller has no Linux version. Standard UVC,
V4L2, UAC, ALSA, and PipeWire behavior is therefore the primary interoperability path. The limited vendor-control work
exists to let owners use documented hardware functions from Linux, not to reproduce or replace the expressive content
of Link Controller.

## Release-review finding

The repository review found original Rust code, documentation, strict functional profiles, synthetic fixtures, and
sanitized, bounded hardware observations. It found no redistributed Insta360 controller executable or source code,
firmware image, artwork, user-interface asset, machine-learning model, credential, account data, or raw packet capture.
The built-in profiles encode only the minimum functional selectors, masks, enum/scalar values, fixed transport bytes,
and timing/verification conditions needed for compatible operation on an exact device and firmware.
Recorded probe bundles include bounded kernel-provided USB descriptor blobs without expanded USB string values; their
manifests describe redaction and checksums.

On that evidence, the project does not contain an obvious copied proprietary work or bundled firmware. This is a
bounded provenance conclusion, not a guarantee that every use, research act, package, name, or contribution is lawful.

## France and EU interoperability context

The maintainer's working legal baseline is France and the European Union:

- [French Intellectual Property Code Article L122-6-1](https://www.legifrance.gouv.fr/loda/article_lc/LEGIARTI000044365559/)
  permits a lawful software user to observe, study, or test its functioning while performing acts the user is entitled
  to perform. It also provides a narrower exception for reproduction or translation of code indispensable to obtain
  otherwise unavailable information needed for interoperability of independently created software, subject to strict
  limits on who acts, what parts are examined, how the information is used or shared, and substantial similarity.
  Contrary contract terms are void for the provisions identified in that article, but its conditions still matter.
- [French Commercial Code Article L151-3](https://www.legifrance.gouv.fr/codes/article_lc/LEGIARTI000037266559/)
  treats independent discovery and observation, study, disassembly, or testing of a publicly available or lawfully
  possessed product as lawful ways to obtain a trade secret, subject to contractual restrictions on obtaining it.
- In [SAS Institute Inc. v World Programming Ltd., C-406/10](https://eur-lex.europa.eu/legal-content/EN/TXT/?uri=CELEX:62010CJ0406),
  the Court of Justice of the European Union held that program functionality, programming languages, and data-file
  formats are not, as such, protected expression of a computer program. Original source/object code and other creative
  expression remain protected, and copying a manual or other work can raise separate issues.
- [EU Trade Mark Regulation 2017/1001, Article 14](https://eur-lex.europa.eu/eli/reg/2017/1001)
  limits a trademark owner's ability to prohibit necessary referential use identifying the owner's goods or their
  intended purpose when the use follows honest commercial practices.

These authorities support a carefully limited independent interoperability effort, not a blanket right to copy,
publish confidential material, evade access controls, ignore a licence, or use a trademark misleadingly. People
outside France/EU must evaluate their own law. Even within France/EU, the facts of possession, applicable terms,
necessity, access method, purpose, and distributed material can change the analysis.

## Contribution provenance rules

Contributors must keep the project independently implementable and auditable:

1. Use only hardware and software that you lawfully possess or are authorized to test. Record the exact camera
   identity, firmware, controller version, source of the software/firmware, and terms presented for that version.
2. Prefer public standards, runtime descriptors, public vendor documentation, and black-box input/output observation.
   Collect only the minimum information necessary to establish one interoperable behavior.
3. Do not submit proprietary source or object code, decompiled code, firmware images, controller binaries, UI assets,
   documentation text, models, keys, credentials, account data, or confidential material.
4. Do not submit raw USB captures. Keep originals private; derive minimal normalized or synthetic fixtures, remove
   serials and host/account identifiers, exclude audio/video and unrelated traffic, and document origin and
   sanitization. A maintainer must review any hardware-derived byte sequence before it enters the repository.
5. Do not bypass authentication, encryption, signature verification, secure boot, DRM, rate limits, device access
   controls, or another technical protection measure. Do not detach drivers, reset USB, force bootloader modes, or
   probe firmware/calibration/flash/mechanical write classes through this project.
6. Limit interoperability findings to this project's compatible implementation and testing. Do not use or disclose
   them to create substantially similar protected expression or for an unrelated purpose.
7. Use product and company names only as necessary factual references. Do not use vendor logos or imply affiliation,
   certification, sponsorship, or endorsement.

The detailed capture and sanitization procedure is in [Safe UVC Extension Unit research](xu-research.md). If provenance
is uncertain, keep the material out of the repository and ask the maintainers to review a metadata-only description
before sharing bytes.

## Firmware and user content

`linkctl` does not bundle, mirror, scrape, or download firmware. Its staging command accepts an official file supplied
by the user and copies it to an already mounted, exact-match U-Disk volume using the vendor's documented manual
workflow. Insta360's [firmware instructions](https://onlinemanual.insta360.com/link2cpro/en-us/faq/operation-guide/firmware)
describe downloading the official file, manually entering U-Disk mode, copying it to the `INSTA360` drive, and
reconnecting the camera. The user remains responsible for obtaining and using the file under applicable terms.

Video, audio, snapshots, presets, logs, and diagnostic bundles belong to their respective users and may contain
personal data or third-party rights. Obtain consent, provide notices, secure files, and follow workplace/recording law.
The project's redaction defaults reduce accidental disclosure but do not make every artifact safe to publish.

## Licences, trademarks, patents, and contracts

The MIT and Apache-2.0 licences cover only `linkctl` contributions distributed under them. They do not license
Insta360 software, firmware, trademarks, patents, documentation, media, or other third-party material. Apache-2.0
contains a contributor patent grant for covered contributions; it is not a patent clearance for the hardware or its
vendor protocols.

A contract or EULA can impose obligations beyond copyright, and the French trade-secret rule cited above expressly
recognizes contractual restrictions in its own context. Do not assume purchase of a camera, access to a download, or
an interoperability exception waives every agreed term. Distributors should preserve this notice, the unofficial
status, third-party attributions, and the source licences, and should perform their own trademark and patent review.
