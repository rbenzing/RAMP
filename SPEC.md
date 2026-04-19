# RAMP — PROJECT SPECIFICATION (v2.1 — Deterministic Execution & Systems Contract)

---

# 1. CORE SYSTEM MODEL

## 1.1 System Definition

RAMP is a deterministic orchestration system defined as:

STATE + EVENT → NEW STATE + SIDE EFFECTS

All system behavior MUST conform to this model.

---

## 1.2 Global State Structure

The system state MUST always be representable as:

* services:

  * apache: ServiceState
  * mysql: ServiceState
  * php: ServiceState
* config: RampConfig
* ports: PortState
* ui: UiState
* last_error: Error | null
* desired_state:

  * apache: DesiredServiceState
  * mysql: DesiredServiceState
  * php: DesiredServiceState

---

## 1.3 State Ownership Rules

* State is owned exclusively by the reducer
* State is immutable outside the reducer
* No shared mutable references allowed

---

# 2. SERVICE STATE MACHINE

## 2.1 Allowed States

Stopped
Starting
Running
Stopping
Crashed
Error

---

## 2.2 Transition Rules

Stopped → START → Starting
Starting → PROCESS_READY → Running
Starting → PROCESS_EXIT → Crashed
Running → STOP → Stopping
Stopping → PROCESS_EXIT → Stopped
Running → PROCESS_EXIT → Crashed
Crashed → AUTO_RETRY → Starting
Any → FATAL_ERROR → Error

---

## 2.3 Invalid Transitions

Any undefined transition MUST:

* be rejected
* emit ERROR event
* not mutate state

---

# 3. EVENT SYSTEM

## 3.1 Event Source Types

* IPC (frontend commands)
* OS process notifications
* Health check workers
* Timer ticks

---

## 3.2 Allowed Events

START_SERVICE
STOP_SERVICE
RESTART_SERVICE
PROCESS_EXIT
PROCESS_READY
HEALTH_CHECK_PASS
HEALTH_CHECK_FAIL
PORT_CONFLICT_DETECTED
CONFIG_RELOADED
FATAL_ERROR
TICK

---

## 3.3 Event Rules

* Events MUST be processed in FIFO order
* No event may bypass the queue
* Events MUST be idempotent where applicable

---

# 4. EVENT LOOP ARCHITECTURE

## 4.1 Execution Model

* Single-threaded reducer loop
* Multi-producer event queue
* Deterministic ordering guaranteed

---

## 4.2 Processing Cycle

1. Dequeue event
2. Apply reducer(state, event)
3. Generate side effects
4. Execute side effects
5. Emit follow-up events

---

## 4.3 Side Effect Rules

Side effects MAY include:

* spawning processes
* killing processes
* file I/O
* network probes

Side effects MUST:

* never mutate state directly
* emit results as events

---

# 5. PROCESS MANAGEMENT (WINDOWS GUARANTEES)

## 5.1 Job Object Requirement

All service processes MUST:

* be attached to a Windows Job Object
* be controlled at Job level, not PID level

---

## 5.2 Guarantees

* No orphaned child processes
* Full process tree termination
* Deterministic cleanup

---

## 5.3 Process Rules

* One Job Object per service
* PID tracking is advisory only
* No external processes allowed outside Job control

---

# 6. SERVICE STARTUP SPECIFICATION

## 6.1 Apache Startup

1. Validate config exists
2. Pre-check port availability
3. Spawn httpd.exe -DFOREGROUND
4. Attach to Job Object
5. Begin readiness checks
6. If ready → PROCESS_READY
7. If exit → PROCESS_EXIT

---

## 6.2 MySQL Startup

1. Validate config exists
2. Ensure data directory exists
3. If first run → initialize database
4. Spawn mysqld --console
5. Attach to Job Object
6. Begin readiness checks
7. If ready → PROCESS_READY
8. If exit → PROCESS_EXIT

---

## 6.3 PHP Startup

1. Validate php-cgi.exe exists
2. Pre-check FastCGI port availability
3. Spawn php-cgi.exe -b 127.0.0.1:{port}
4. Attach to Job Object
5. Begin readiness checks (TCP connect)
6. If ready → PROCESS_READY
7. If exit → PROCESS_EXIT

---

# 7. SERVICE READINESS CONTRACTS

## 7.1 Apache Ready Condition

* Port bound successfully
* HTTP response received
* Response code 200–399
* Response signature matches Apache

---

## 7.2 MySQL Ready Condition

* TCP connection succeeds
* MySQL protocol handshake completes

---

## 7.3 PHP Ready Condition

* TCP connection to FastCGI port succeeds

---

## 7.4 Timeout Rules

* Apache: 3 seconds
* MySQL: 5 seconds
* PHP: 5 seconds

Timeout failure triggers PROCESS_EXIT or HEALTH_CHECK_FAIL

---

# 8. SERVICE SHUTDOWN SPECIFICATION

## 8.1 Shutdown Sequence

1. Set state = Stopping
2. Send graceful stop signal
3. Wait up to 5 seconds
4. If still alive → force kill Job
5. Confirm termination
6. Emit PROCESS_EXIT

---

## 8.2 Guarantees

* No lingering processes
* No partial shutdown

---

# 9. HEALTH CHECK SYSTEM

## 9.1 Execution

* Runs every TICK (2 seconds default)

---

## 9.2 Apache Check

* HTTP GET request
* Success = 200–399 response

---

## 9.3 MySQL Check

* TCP connect
* Optional handshake validation

---

## 9.4 Failure Detection

* 3 consecutive failures → HEALTH_CHECK_FAIL

---

## 9.5 Recovery

If auto_restart enabled:

* emit AUTO_RETRY
* follow retry policy

---

# 10. RETRY POLICY

## 10.1 Backoff Schedule

1s → 2s → 4s → 8s → STOP

---

## 10.2 Rules

* Max 4 retries
* After max retries → Error state
* No infinite retry loops allowed

---

# 11. PORT MANAGEMENT

## 11.1 Validation Strategy

* Pre-check via TCP bind test
* Runtime validation via process feedback

---

## 11.2 Failure Handling

If bind fails:

* emit PORT_CONFLICT_DETECTED
* transition to Error

---

## 11.3 Constraints

* Ports must be unique across services
* Ports must be within valid range

---

# 12. CONFIGURATION SYSTEM

## 12.1 Source of Truth

* ramp.toml is authoritative

---

## 12.2 Write Strategy

* Write temp file
* fsync
* atomic rename

---

## 12.3 Validation Rules

* Entire config validated before write
* Invalid config rejected completely
* No partial updates allowed

---

## 12.4 Recovery

* Last valid config always retained
* Corrupt config triggers fallback to defaults

---

# 13. SECURITY CONSTRAINTS

## 13.1 Execution Safety

* Only absolute paths allowed
* No PATH-based execution
* Environment variables sanitized

---

## 13.2 Network Safety

* Bind to 127.0.0.1 only
* No external exposure

---

## 13.3 Filesystem Safety

All filesystem interactions MUST be deterministic, atomic, and failure-safe.

### Rules

- All file writes MUST use atomic write strategy:
  - write to temporary file
  - fsync
  - atomic rename (replace existing)

- Partial writes are strictly forbidden

- Configuration files MUST never be modified in-place

- All paths MUST be absolute and normalized before use

- Directory traversal (e.g., `..`) MUST be rejected

- Symbolic links MUST NOT be followed for critical paths:
  - config directory
  - binaries
  - data directory

- File permissions MUST be restricted where applicable:
  - MySQL data directory must not be world-readable
  - config files must not be executable

---

## 13.4 Binary Integrity & Execution Safety

### Binary Validation

Before execution, all service binaries MUST be validated:

- File existence check
- Expected directory validation (must reside inside install_dir)
- Optional checksum validation (recommended for future hardening)

### Execution Rules

- Only absolute paths are allowed
- No reliance on PATH environment variable
- Working directory MUST be explicitly set to service root
- Environment variables MUST be sanitized:
  - remove user-injected PATH overrides
  - restrict execution context

### Forbidden

- Executing user-provided binaries
- Executing from temporary directories
- Dynamic binary discovery at runtime

---

## 13.5 Process Isolation Guarantees

Each service MUST operate within its own isolated execution boundary.

### Requirements

- Each service is assigned a dedicated Windows Job Object
- All child processes MUST inherit the Job Object
- No process may escape its assigned Job

### Guarantees

- Killing a Job Object terminates entire process tree
- Prevents zombie/orphan processes
- Ensures deterministic cleanup

### Failure Mode Handling

If Job Object assignment fails:

- Service MUST NOT start
- Emit: PROCESS_SPAWN_FAILED
- Transition service state to Error
- Log failure with full context

---
