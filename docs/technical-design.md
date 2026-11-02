# Technical design

**English | [简体中文](technical-design.zh-CN.md)**

This page describes the data flow and file-safety boundaries. See [implementation status](implementation-status.md), [release guide](release.md), and the [acceptance checklist](acceptance-checklist.md) for evidence and remaining work.

## Workflow and data flow

The bottom navigation lets users open a stage directly. Dashed arrows show optional transfer of a review manifest; stages do not have to run in sequence.

```mermaid
flowchart LR
    Source[Choose source folders] --> Scan[1 Scan files]
    Scan --> Route[2 Set organization rules]
    Route --> Copy[3 Copy into folders]
    Copy --> Verify[4 Verify copied files]
    Source -. Choose folders .-> Metadata[5 Check metadata]
    Source -. Choose folders .-> Duplicate[6 Review duplicate candidates]
    Duplicate -. Optional review manifest .-> Backup[7 Back up to multiple targets]
    Duplicate -. Optional review manifest .-> Archive[8 Create encrypted archive]
    Source -. Choose folders independently .-> Backup
    Source -. Choose folders independently .-> Archive
```

Scans and task status usually live only in the current session. Private local checkpoints can restore an unfinished organization copy, backup mapping, archive inputs, or duplicate-review manifest. They do not store archive passwords or old success results. Resumed work rereads files and verifies their current contents.

## Organization copy and overwrite protection

```mermaid
sequenceDiagram
    actor User
    participant App
    participant Source as Source file
    participant Temp as Temporary file
    participant Dest as Destination folder

    User->>App: Confirm the copy plan
    App->>Source: Check the scan-time file state
    App->>Temp: Bounded streaming copy and SHA-256
    App->>Source: Reread source and calculate SHA-256
    App->>Temp: Reread temporary file and calculate SHA-256
    App->>Dest: Commit only if hashes match and path is unused
    Dest-->>App: Report if destination exists or differs
```

Originals are not moved or deleted. A copy is first written to a temporary file in the destination folder. The app commits it only after source and temporary-file contents match and the destination can be created safely. It will not retry a failure by overwriting an existing file.

## Encrypted archive verification

```mermaid
flowchart TD
    Input[Scan source and verify inputs] --> Build[Write encrypted temporary 7z archive]
    Build --> Split[Split archive into configured volumes]
    Split --> Reassemble[Read joined volumes and archive manifest]
    Reassemble --> Compare{Paths and SHA-256 match?}
    Compare -->|No| Reject[Do not commit]
    Compare -->|Yes| Optional{Restore verification enabled?}
    Optional -->|Yes| Extract[Restore to isolated folder and reread]
    Optional -->|No| Commit[Commit volumes without overwriting]
    Extract -->|Pass| Commit
    Extract -->|Fail| Reject
```

Restoring an interrupted archive task requires entering the password again; the app rescans, rebuilds, and verifies the archive from the beginning. Volume splitting and isolated restore checks need extra temporary disk space. See the [release guide](release.md) for limits and platform acceptance status.

## Related pages

- [Implementation status and verification](implementation-status.md)
- [Build, release status, and known limits](release.md)
- [Clean-machine acceptance checklist](acceptance-checklist.md)
