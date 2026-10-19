## Purpose

Defines how package payload archives are validated and prepared without executing vendor applications.

## ADDED Requirements

### Requirement: Standalone gzip executables are installed decompressed
The system SHALL decode a payload that consists of one standalone gzip stream before installing it as the declared executable artifact.

#### Scenario: A standalone gzip executable runs
- **WHEN** a package declares a standalone gzip stream as an executable artifact
- **THEN** the system installs the decompressed stream
- **AND** the installed executable starts and reports its version

#### Scenario: Compressed documentation remains unchanged
- **WHEN** a package declares a gzip-compressed documentation file
- **THEN** the system preserves that file bytes
- **AND** the file remains valid gzip data
