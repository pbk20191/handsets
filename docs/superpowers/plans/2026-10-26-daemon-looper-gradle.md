# 데몬 단일 루퍼 + Gradle 빌드 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** on-device 데몬을 MainLooper 하나 + 작은 Work 풀 구조의 Kotlin 코루틴 모델로 바꾸고, Gradle이 `build/hs.jar`를 만드는 정식 빌드가 되게 한다. 와이어 프로토콜은 바이트 단위로 유지.

**Architecture:** `Looper.loop()`를 도는 main 스레드가 `MessageQueue.addOnFileDescriptorEventListener`(epoll)로 소켓·파이프를 감시하고, 연결마다 `Dispatchers.Main` 위의 코루틴 하나가 `readFrame → dispatch → respond`를 suspend로 처리한다. CPU/장시간 binder 작업만 `Work` 디스패처로 보낸다. 중간 단계에서는 아직 포팅 안 된 블로킹 Java 핸들러를 `Legacy` 디스패처 + 블로킹 스트림 어댑터로 돌려 매 태스크마다 jar가 동작하게 하고, 마지막에 `Legacy`를 제거한다.

**Tech Stack:** Kotlin 2.2.21 (JVM target 17), kotlinx-coroutines-core/android (최신 안정 1.10.x), Gradle(Kotlin DSL, 기존 wrapper), SDK R8(`build-tools/36.0.0/lib/d8.jar`), JUnit5 + kotlinx-coroutines-test (JVM 단위 테스트), Python 3 (`scripts/wire-smoke.py`, 기기 통합 테스트), AVD `Medium_Phone`(API 36).

**Spec:** `docs/superpowers/specs/2026-10-25-daemon-looper-gradle-design.md`

## Global Constraints

- 와이어 프로토콜(`docs/wire.md`): verb, 인자, 응답 바이트, `uint32 BE length + payload` 프레이밍, `ERR:` 문자열, 스트림 `len=0` 종결자, `__exit__ N` 트레일러 — 전부 기존과 동일. `MAX_CMD = 256`, 위반 시 `BAD_ARG:bad-length:<len>` 후 연결 종료.
- 엔트리: `dev.handsets.daemon.Main.main(String[])`, `--port=N`(기본 9008), 127.0.0.1 바인드, backlog 8, `SO_REUSEADDR`, `TCP_NODELAY`. stdout에 `hsd listening on 127.0.0.1:<port>` 와 `hsd ready (sdk=<N>)` 출력 유지.
- 산출물: `build/hs.jar` (dex만), `--min-api 28`, R8 `--release`. 크기 목표: 기준 jar 대비 +300KB 이내, 초과 시 보고.
- R8: SDK `build-tools/<ver>/lib/d8.jar`의 `com.android.tools.r8.R8`를 `JavaExec`로 실행. 기본 build-tools `36.0.0`, `-PbuildTools=`로 변경.
- 루퍼(main)에서 금지: 덤프, 인코딩, `Thread.sleep`, 블로킹 소켓/파이프 I/O, 상한 없는 binder 호출. 애매하면 `Work`.
- 스레드 최종 상태: main + Work 풀(`min(4, availableProcessors)`) (+ 프레임워크 binder/MediaCodec 스레드). `hs-ua`는 main looper 시도가 실패할 때만 예외로 허용하고 보고.
- 이미지/상태 스트림(`stream`, `stream_tilejpeg`, `state_watch`, `clip_watch`)은 최신 프레임만 유지. `stream_h264`는 공유 전까지 프레임 무손실 + 연결별 backpressure.
- 호스트 코드(`handsets-cli`, `handsets-tui`, `handsets-viewer`, `bindings/`) 수정 금지.
- 검증 기기: AVD `Medium_Phone` (API 36, x86_64) 하나. adb: `E:\packages\Android\Sdk\platform-tools\adb.exe`.

## Review Focus

1. **TCP 단편화:** 명령 프레임의 4바이트 길이/페이로드가 여러 read로 쪼개져 도착 → 하나의 명령으로 정상 처리. (Task 3 `ConnTest.fragmentedFrame`)
2. **스트림 중 클라이언트 강제 종료:** `stream*`/`state_watch`/`logcat`/`shell` 중 소켓이 끊김 → 구독 해제, 서브프로세스 kill, 인코더/VirtualDisplay 해제, 다른 연결 영향 없음. (Task 3 `ConnTest.cancelOnEof`, Task 9 smoke `disconnect_cleanup`)
3. **느린/읽지 않는 클라이언트:** 스트림을 열고 읽지 않는 클라이언트가 있어도 다른 연결의 `ping` p95 < 50ms. (Task 3 `ConnTest.latestOnlyReplaces`, Task 8 smoke `slow_client`)
4. **설치 중 업로드 중단:** `install size=N` 후 N바이트를 다 보내기 전에 끊김 → 세션 abandon, 데몬 정상. (Task 6 smoke `install_truncated`)
5. **UTF-8 / 경계 길이 명령:** CJK `clip_set`, `text`, 길이 정확히 256/257 프레임 → 기존과 같은 응답. (Task 4 `DispatchTest.cjkAndLengthBounds`, smoke 케이스)

---

## File Structure

새 Kotlin 파일 (`src/dev/handsets/daemon/`, 패키지 `dev.handsets.daemon`):

| 파일 | 책임 |
|---|---|
| `Loop.kt` | `Main`/`Work`/`Legacy` 디스패처, 루트 `scope`, `onMain {}` 헬퍼 |
| `IoLoop.kt` | `IoLoop` 인터페이스 + `MessageQueueIoLoop`(Android), fd 획득 `Fds` |
| `Conn.kt` | 비블로킹 연결: suspend 읽기, 출력 큐(backpressure / latest-only), 종료 |
| `HsServer.kt` | accept 루프, 연결별 코루틴 |
| `Dispatch.kt` | verb 디스패치 (기존 `Server.handle` switch + `run*` 포팅) |
| `Args.kt` | 기존 `Server`의 순수 파싱 헬퍼(`strArg`, `longArg`, `positional`, `parseInputArgs`, …) 포팅 |
| `Main.kt` | 엔트리, 초기화 순서, `Looper.loop()` (기존 `Main.java` 대체) |
| `Waits.kt` | `UiEvents` + `WaitRegistry` 대체 (suspend) |
| `StateRefresher.kt` | `Debouncer` + State 리프레시/구독 fan-out |
| `ProcStream.kt` | 서브프로세스 stdout을 IoLoop로 읽어 청크 프레임 전송 |
| `Streams.kt` | `stream`/`stream_tilejpeg`/`stream_h264`/`state_watch`/`clip_watch` 세션 |
| `LegacyStreams.kt` | 임시: `Conn` ↔ 블로킹 `DataInputStream/DataOutputStream` 어댑터 (Task 9에서 삭제) |

삭제: `Server.java`, `Main.java`, `UiEvents.java`, `WaitRegistry.java`, `Streamer.java`, `LocalIntentSender.java` (각 해당 태스크에서).
테스트: `test/dev/handsets/daemon/*Test.kt` (JVM, android 미사용 코드만).
스크립트: `scripts/wire-smoke.py`.

---

### Task 1: Gradle이 `build/hs.jar`를 만든다 (동작 변경 없음)

**Files:**
- Modify: `build.gradle.kts`, `settings.gradle.kts`, `proguard-rules.pro`, `build.sh`, `.gitignore`, `.github/workflows/release.yml`, `README.md`(Daemon IDE setup 섹션 → "Building the daemon"), `docs/architecture.md`(build 줄)
- Untrack: `local.properties` (`git rm --cached`, `.gitignore`에 추가)
- 그대로 포함: 워킹트리의 `Screenshot.java` 수정(`Binders.systemContext()` 사용)

**Interfaces:**
- Produces: `./gradlew hsJar` → `build/hs.jar`; `./gradlew test` (JUnit5, 소스 `test/`); Gradle 속성 `-PandroidCompileSdk=<int>`(기본: 설치된 최신 numeric `android-N`), `-PbuildTools=<ver>`(기본 `36.0.0`). Gradle 내부 출력은 `build-gradle/`.

- [ ] **Step 1:** `build.gradle.kts` 정리 — `compileSdk` 하드코딩(`37.2`) 제거하고 `platforms/android-<N>` 중 최대 정수 N 선택(속성 우선). `java { toolchain 17 }`, `kotlin { jvmToolchain(17) }`, `-no-jdk` 제거(JDK 클래스는 android.jar bootclasspath가 대신하므로 Kotlin은 `compileOnly(files(androidJar))`로 충분). 의존성: `implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:<최신 1.10.x>")`, `testImplementation(kotlin("test"))`, `testImplementation("org.jetbrains.kotlinx:kotlinx-coroutines-test:<같은 버전>")`, JUnit5 platform. `sourceSets.test` → `test/`.
- [ ] **Step 2:** `hsJar` 태스크 등록: `JavaExec`(mainClass `com.android.tools.r8.R8`, classpath `${sdk}/build-tools/${bt}/lib/d8.jar`), args `--release --min-api 28 --lib <androidJar> --pg-conf proguard-rules.pro --output <build-gradle/r8/hs-dex.zip>` + 입력: `jar` 태스크 산출물 + `runtimeClasspath` jar 전체. 이후 `Zip`/`Copy`로 `classes.dex`만 담은 `build/hs.jar` 생성. `assemble`이 `hsJar`에 의존. inputs/outputs 선언해 up-to-date 동작.
- [ ] **Step 3:** `proguard-rules.pro`에 코루틴 규칙 추가: `-keepclassmembers class kotlinx.coroutines.** { volatile <fields>; }`, `-keepnames class kotlinx.coroutines.internal.MainDispatcherFactory {}`, `-keepnames class kotlinx.coroutines.android.AndroidDispatcherFactory {}`, `-keepnames class kotlinx.coroutines.CoroutineExceptionHandler {}`.
- [ ] **Step 4:** `build.sh` → `exec ./gradlew hsJar "$@"` 래퍼 (Windows Git Bash에서도 동작하게 `cd "$(dirname "$0")"` 유지). CI: platforms `android-36`, build-tools `36.0.0`, JDK 17 유지, 단계는 `./build.sh` 그대로.
- [ ] **Step 5: 빌드 확인**
  Run: `./gradlew hsJar && unzip -l build/hs.jar`
  Expected: `BUILD SUCCESSFUL`, 항목은 `classes.dex` 하나. 크기 기록.
- [ ] **Step 6: 기준 jar 보관** — `cp build/hs.jar build/baseline-hs.jar` (이후 모든 비교의 기준. 이 시점 코드는 기존 Java와 동일).
- [ ] **Step 7: 기기 확인** — `emulator -avd Medium_Phone` 기동 후 jar push, `CLASSPATH=/data/local/tmp/hs.jar nohup app_process /system/bin --nice-name=hsd dev.handsets.daemon.Main --port=9008 > /data/local/tmp/hsd.log 2>&1 &`, `adb forward tcp:9008 tcp:9008`, `ping`.
  Expected: `pong`, 로그에 `hsd ready (sdk=36)`.
- [ ] **Step 8: Commit** — `build: gradle produces build/hs.jar via SDK R8`

### Task 2: 와이어 스모크 하네스

**Files:**
- Create: `scripts/wire-smoke.py`

**Interfaces:**
- Produces: CLI
  - `wire-smoke.py run --jar PATH --out results.json [--serial S] [--port 9008] [--only CASE,...]` — jar를 push·기동(Task 1 Step 7과 같은 명령, 기존 hsd는 `pkill -f hsd`로 종료)하고 케이스 실행, 정규화된 응답 저장.
  - `wire-smoke.py diff A.json B.json` — 케이스별 차이 출력, 차이 있으면 exit 1.
  - `wire-smoke.py threads --port 9008` — `pidof hsd` → `/proc/<pid>/task/*/comm` 목록 출력.
  - `wire-smoke.py bench --port 9008 [-n 100]` — `ping`/`tap`/`dump_active`/`screenshot` p50/p95 ms.
- 케이스 이름(이후 태스크가 참조): `ping info dump dump_active screenshot_jpeg screenshot_png tap swipe key text_cjk getprop settings_get wm_info pm_list pm_path state_fields clip_roundtrip_cjk deeplinks providers wait_idle wait_text_ok wait_text_timeout wait_activity tap_and_dump tap_and_settle node_click stream_n stream_h264_n stream_tile_n state_watch_n clip_watch_n logcat_head dumpsys_window shell_echo pull_push install install_truncated len_256 len_257 len_0 parallel_ping_during_wait slow_client disconnect_cleanup`.
- 정규화: JSON의 숫자 중 시간·배터리·메모리·uptime·elapsed 계열 키, `elapsed=`/`x=..`(wait_text 좌표는 유지) 값, 이미지/비디오 바이트는 `{"kind":"jpeg|png|h264|tile","frames":n,"ok":true}` 요약으로 치환. 동시성 케이스는 지표가 임계값 이내인지 `true/false`만 기록(`parallel_ping_during_wait`: wait_for_text timeout_ms=5000 진행 중 다른 소켓 ping < 100ms; `slow_client`: 읽지 않는 stream 소켓 3초 유지 중 ping p95 < 50ms; `disconnect_cleanup`: stream/shell 소켓 강제 종료 후 2초 뒤 스레드 수 ≤ 접속 전 + 0, 그리고 `ping` 정상).
- install 케이스용 APK: 기기에 이미 있는 작은 시스템 APK를 `pm_path`로 찾아 `pull`한 바이트를 사용(외부 파일 불필요).

- [ ] **Step 1:** 스크립트 작성 (표준 라이브러리만; adb 경로는 `ADB` 환경변수 또는 `local.properties`의 `sdk.dir` + `platform-tools/adb`).
- [ ] **Step 2: 자기 일관성 확인**
  Run: `python scripts/wire-smoke.py run --jar build/baseline-hs.jar --out build/smoke-a.json && python scripts/wire-smoke.py run --jar build/baseline-hs.jar --out build/smoke-b.json && python scripts/wire-smoke.py diff build/smoke-a.json build/smoke-b.json`
  Expected: diff exit 0 (정규화가 변동값을 모두 가림). 기준 jar에서 실패하는 동시성 케이스(`slow_client`, `parallel_ping_during_wait` 등)가 있다면 그 결과 그대로 기록 — 기준 동작이므로.
- [ ] **Step 3:** `python scripts/wire-smoke.py threads` 기준 스레드 목록을 `build/threads-baseline.txt`로 저장, `bench` 결과를 `build/bench-baseline.txt`로 저장.
- [ ] **Step 4: Commit** — `test: wire smoke harness for daemon protocol parity`

### Task 3: 이벤트 루프 코어 (`Loop`, `IoLoop`, `Conn`)

**Files:**
- Create: `src/dev/handsets/daemon/Loop.kt`, `IoLoop.kt`, `Conn.kt`
- Test: `test/dev/handsets/daemon/ConnTest.kt`, `test/dev/handsets/daemon/SelectorIoLoop.kt`(테스트용 IoLoop 구현)

**Interfaces:**
- Produces:
  - `Loop.kt`: `object Loop { val Main: CoroutineDispatcher; val Work: CoroutineDispatcher; val Legacy: CoroutineDispatcher; val scope: CoroutineScope; fun init(main: CoroutineDispatcher = Dispatchers.Main.immediate) }` — `Work` = 고정 `min(4, cpu)` 데몬 스레드 풀(`hs-work-N`) `.asCoroutineDispatcher()`, `Legacy` = 캐시드 풀(`hs-legacy-N`, Task 9에서 제거). `scope = CoroutineScope(SupervisorJob() + Main + CoroutineExceptionHandler{ 로그 })`. `init`이 `Dispatchers.Main` 해석 실패 시 `Handler(Looper.getMainLooper()).asCoroutineDispatcher("hs-main")`로 대체하고 어느 쪽인지 stderr에 `hsd dispatcher: main=<kind>` 출력.
  - `IoLoop.kt`: `interface IoLoop { fun watch(fd: FileDescriptor, events: Int, cb: (fd: FileDescriptor, events: Int) -> Int); fun unwatch(fd: FileDescriptor) }` (events/반환값 의미는 `MessageQueue.OnFileDescriptorEventListener`와 동일: `EVENT_INPUT=1, EVENT_OUTPUT=2, EVENT_ERROR=4`, 0 반환 = 해제). `class MessageQueueIoLoop(queue: MessageQueue) : IoLoop`. `object Fds { fun of(ch: SelectableChannel): FileDescriptor }` — Android `SocketChannelImpl`/`ServerSocketChannelImpl`의 `getFD()`를 리플렉션으로 호출.
  - `Conn.kt`: `class Conn(ch: SocketChannel, io: IoLoop) { suspend fun readInt(): Int; suspend fun readFully(n: Int): ByteArray; suspend fun readChunk(max: Int): ByteArray? /* null=EOF */; suspend fun send(payload: ByteArray) /* 프레임(len+payload) 큐잉, 큐 바이트 > OUT_LIMIT(4 MiB)이면 drain까지 suspend */; suspend fun sendRaw(bytes: ByteArray) /* 길이 접두 없이 */; fun sendLatest(slot: Int, payload: ByteArray) /* 같은 slot의 아직 안 나간 프레임을 교체 */; suspend fun flush(); val job: Job /* EOF/에러/close 시 cancel */; fun close() }`. 읽기 EOF는 `EOFException`을 던진다.

- [ ] **Step 1: 실패하는 테스트 작성** — `ConnTest` (JVM, `SelectorIoLoop`가 별도 스레드에서 `java.nio.channels.Selector`로 `IoLoop` 계약을 흉내, 디스패처는 단일 스레드 executor):
  - `fragmentedFrame`: 클라이언트가 `[0,0,0,4]`를 2바이트씩, `"ping"`을 1바이트씩 보냄 → `readInt()==4`, `String(readFully(4))=="ping"`.
  - `sendFramesInOrder`: `send("a")`, `send("bb")` → 클라이언트가 `00000001 61 00000002 6262` 수신.
  - `backpressureSuspends`: 클라이언트가 읽지 않을 때 `send`를 반복하면 누적 큐 바이트가 `OUT_LIMIT`+최대 프레임 1개를 넘지 않음(초과 시점에 suspend 확인 — `withTimeoutOrNull(200){ send(big) } == null`).
  - `latestOnlyReplaces`: 읽지 않는 클라이언트에 `sendLatest(0, f1..f50)` → 클라이언트가 읽기 시작하면 받은 프레임 수 ≤ 3 이고 마지막이 `f50`.
  - `cancelOnEof`: 클라이언트 close → `conn.job.isCancelled` 가 1초 내 true, 진행 중 `readInt()`는 `EOFException`/취소로 끝남.
- [ ] **Step 2:** Run: `./gradlew test --tests '*ConnTest*'` — Expected: FAIL (클래스 없음).
- [ ] **Step 3:** `Loop.kt`, `IoLoop.kt`, `Conn.kt` 구현. `Conn`은 읽기 대기 시 `EVENT_INPUT`, 출력 큐가 비어 있지 않을 때만 `EVENT_OUTPUT`을 watch(쓰기 가능할 때마다 non-blocking write). 소켓은 `configureBlocking(false)`, `TCP_NODELAY`.
- [ ] **Step 4:** Run: `./gradlew test --tests '*ConnTest*'` — Expected: PASS.
- [ ] **Step 5: Commit** — `daemon: looper io core (IoLoop, Conn, dispatchers)`

### Task 4: Kotlin 서버·디스패치·Main (루퍼 기동)

**Files:**
- Create: `HsServer.kt`, `Dispatch.kt`, `Args.kt`, `Main.kt`, `LegacyStreams.kt`
- Delete: `Server.java`, `Main.java`
- Modify: `Binders.java`(`prepareMainLooper` 호출 제거 — Main이 담당), `Screenshot.java`(미러 HandlerThread는 그대로; Task 8에서 처리)
- Test: `test/dev/handsets/daemon/ArgsTest.kt`, `test/dev/handsets/daemon/DispatchTest.kt`

**Interfaces:**
- Consumes: Task 3 `Loop`, `IoLoop`, `Fds`, `Conn`.
- Produces:
  - `class HsServer(port: Int, io: IoLoop, dispatch: Dispatch) { fun start() }` — `ServerSocketChannel` non-blocking, accept마다 `Loop.scope.launch { dispatch.session(conn) }`.
  - `class Dispatch(h: Handlers) { suspend fun session(c: Conn) }` — 기존 `Server.handle` 루프와 같은 순서: `readInt` → 길이 검사 → `readFully` → UTF-8 → `trim` → head 분기. 스트림 verb 후 `return`(연결 종료) / 청크 verb 후 `continue` 의미 유지.
  - `class Handlers` (Kotlin, 기존 `Server.Handlers` 필드 동일 이름, `lateinit var`).
  - `Args.kt`: 기존 `Server` private static 헬퍼를 이름 그대로 top-level `internal fun`으로 (`strArg`, `strArgOr`, `longArg`, `extractKey`, `removeKey`, `positional`, `positionalPair`, `afterHead`, `parseInputArgs`, `parseScreenshotArgs`, `parseFileArgs`, `parseAmArgs`, `clampQuality`, `utf8`, `errBytes`, `textSelector`). `parseScreenshotArgs`/`textSelector`처럼 android 타입을 쓰는 것은 별도 파일로 분리 가능.
  - `LegacyStreams.kt`: `fun Conn.blockingOut(): DataOutputStream` (쓰기 → `runBlocking(Loop.Main)`이 아닌 `Loop.scope.launch{ sendRaw }` + 용량 세마포어로 Legacy 스레드만 블로킹), `fun Conn.blockingIn(): DataInputStream` (Legacy 스레드에서 `runBlocking { readChunk }`). 아직 포팅 안 된 핸들러(`Files.pull/push`, `Installer`, `Dumpsys`, `Logcat`, `ShellExec`, `Lifecycle`, `Clipboard.watch`, `Streamer`, `TileStreamer`, `H264Streamer`, `state_watch`, waits)는 `withContext(Loop.Legacy) { 기존 Java 호출(blockingIn/Out) }`. `byte[]`를 반환하는 동기 핸들러 중 덤프·provider·pm_list·notifications·screenshot·deeplinks·location·settings는 `withContext(Loop.Work)`, 나머지 짧은 것(`ping`, `info`, `getprop`, `keyframe`, `wm_info`, `state <field>` 캐시 읽기)은 main에서 직접.
  - `Main.kt`: `object Main { @JvmStatic fun main(args: Array<String>) }` 순서: hidden-API 해제 → `Looper.prepareMainLooper()` → `Loop.init()` → `HsServer.start()`(listening 출력) → `Loop.scope.launch { initDaemon() }` → `Looper.loop()`. `initDaemon`: `withContext(Work){ UiAutomation(mainLooper, conn) 생성 + connectWithRetry + setServiceInfo/flags + setRunAsMonkey }` → main에서 핸들러 생성 → warmup(`launch(Work){ dumper.dumpActive() }`) → `hsd ready (sdk=N)` 출력 → `ready.complete(Unit)`. `Dispatch.session`은 `ping`/`quit` 외 verb 처리 전 `ready.await()`. UA connect가 5초 내 실패/교착이면 `hs-ua` HandlerThread looper로 재생성하고 stderr `hsd ua-looper=thread`(성공 시 `=main`) 출력. 셧다운 훅(UA disconnect) 유지. `quit`은 `bye` 전송·flush 후 `exitProcess(0)`.

- [ ] **Step 1: 실패하는 테스트** — `ArgsTest`: 기존 Java 동작에서 뽑은 표 기반 케이스 (`strArg("text text=\"a b\"","text")=="a b"`, `longArg("wait_for_idle idle_ms=300","idle_ms",200)==300`, `positional("pm_path com.x")=="com.x"`, `parseInputArgs("tap x=10 y=20")` x/y, `removeKey`/`extractKey` 따옴표 처리). 기대값은 구현 전에 기존 `Server.java` 코드를 읽고 손으로 산출해 고정. `DispatchTest.cjkAndLengthBounds`: 가짜 `Handlers`로 `Dispatch.session`을 `SelectorIoLoop` 위에서 돌려 `len=0`→`ERR:BAD_ARG:bad-length:0`+종료, `len=257`→`ERR:BAD_ARG:bad-length:257`+종료, `len=256` 정상 처리, `ping`→`pong`. (`errBytes`의 정확한 접두는 기존 코드 기준.)
- [ ] **Step 2:** Run: `./gradlew test` — Expected: FAIL.
- [ ] **Step 3:** `Args.kt`, `Dispatch.kt`, `HsServer.kt`, `LegacyStreams.kt`, `Main.kt` 구현, `Server.java`/`Main.java` 삭제. `Dispatch`는 기존 switch의 모든 case를 동일 head 문자열로 포함(누락 시 `default` 응답도 기존과 동일).
- [ ] **Step 4:** Run: `./gradlew test` — Expected: PASS.
- [ ] **Step 5: 기기 패리티**
  Run: `./gradlew hsJar && python scripts/wire-smoke.py run --jar build/hs.jar --out build/smoke-t4.json && python scripts/wire-smoke.py diff build/smoke-a.json build/smoke-t4.json`
  Expected: exit 0. 기기 로그에 `hsd dispatcher: main=...`, `hsd ua-looper=...` 확인 후 결과를 커밋 메시지에 기록.
- [ ] **Step 6: Commit** — `daemon: kotlin server on main looper with legacy bridges`

### Task 5: UI 이벤트·대기·State·Input을 main으로

**Files:**
- Create: `Waits.kt`, `StateRefresher.kt`, `Input.kt`(기존 `Input.java` 대체)
- Delete: `UiEvents.java`, `WaitRegistry.java`, `Input.java`
- Modify: `State.java`(refresher 스레드·HandlerThread 3개·`ensureRefresher`/`markDirty`/`dirtyLock` 제거, 리스너 등록을 주입받은 main `Handler`로; `computeDevice()`는 그대로 두고 package-private로), `Dispatch.kt`(waits/tap_and_*/state/state_watch/input을 Legacy에서 suspend 호출로)
- Test: `test/dev/handsets/daemon/WaitsTest.kt`, `test/dev/handsets/daemon/DebouncerTest.kt`

**Interfaces:**
- Consumes: `Loop`, `Conn.sendLatest`.
- Produces:
  - `class Waits(clock: () -> Long = SystemClock::uptimeMillis) { fun onEvent(type: Int); fun touch(); suspend fun awaitIdle(idleMs: Long, timeoutMs: Long): Long /* -1=timeout */; suspend fun awaitPredicate(timeoutMs: Long, check: suspend () -> Boolean): Long /* 이벤트마다 재평가, -1=timeout */ ; fun subscribe(c: (AccessibilityEventType: Int) -> Unit) }` — `UiAutomation.setOnAccessibilityEventListener`는 `Main.kt`가 `Waits.onEvent(ev.eventType)`로 연결 (UA 콜백 스레드가 main이 아니면 `Handler(main).post`). `wait_for_text`의 노드 검색 predicate는 `withContext(Work)`.
  - `class Debouncer(scope: CoroutineScope, debounceMs: Long, heartbeatMs: Long, action: suspend () -> Unit) { fun mark() }` — `mark()`마다 debounce 재시작, 마지막 실행 후 `heartbeatMs` 경과 시에도 실행. `DEBOUNCE_MS`/`HEARTBEAT_MS` 값은 `State.java` 기존 상수 그대로.
  - `class StateRefresher(state: State, scope) { fun ensureStarted(); fun subscribe(l: (ByteArray) -> Unit); fun unsubscribe(l) ; fun deviceFresh(): suspend ByteArray }` — `action` = `withContext(Work){ state.computeDevice() }` → main에서 캐시 저장 + 구독자 호출.
  - `Input.kt`: `class Input(ua: UiAutomation)` 같은 메서드 이름, `swipe`/`text`/`keyPress` 등 시간 간격이 있는 것은 `suspend`(`delay`), `injectInputEvent`는 `withContext(Work)`.
- [ ] **Step 1: 실패하는 테스트** (kotlinx-coroutines-test `runTest`, 가상 시간, 가상 clock = `testScheduler.currentTime`):
  - `WaitsTest.idleReturnsAfterQuiet`: `touch()` 후 `awaitIdle(200, 5000)` → 가상 시간 200ms에 반환값 ≥200.
  - `WaitsTest.eventsPostponeIdle`: 100ms마다 `onEvent` 5회 → 반환 시각 = 마지막 이벤트 + 200.
  - `WaitsTest.idleTimeout`: 50ms마다 이벤트 계속 → `awaitIdle(200, 1000) == -1L`, 가상 1000ms에 반환.
  - `WaitsTest.predicateRecheckedOnEvent`: check가 3번째 이벤트 후 true → 그 시각의 경과 반환.
  - `DebouncerTest.coalesces`: 30ms 간격 `mark()` 5회 → action 1회, 마지막 mark + debounce 시각.
  - `DebouncerTest.heartbeat`: mark 없이 `heartbeatMs * 2` 경과 → action 2회.
- [ ] **Step 2:** Run: `./gradlew test --tests '*WaitsTest*' --tests '*DebouncerTest*'` — FAIL.
- [ ] **Step 3:** 구현 + `State.java` 정리 + `Dispatch` 연결(`state_watch`는 `StateRefresher.subscribe { conn.sendLatest(0, it) }`, 연결 job 종료 시 unsubscribe).
- [ ] **Step 4:** Run: `./gradlew test` — PASS.
- [ ] **Step 5:** 기기 패리티 — Task 4 Step 5와 같은 명령(`smoke-t5.json`). Expected: diff exit 0, `parallel_ping_during_wait` true.
- [ ] **Step 6: Commit** — `daemon: event-driven waits, state refresh and input on main looper`

### Task 6: 설치·패키지·파일 전송을 suspend로

**Files:**
- Create: `Transfers.kt`
- Delete: `LocalIntentSender.java`
- Modify: `Installer.java`, `Pm.java`, `Files.java` → 세션 생성/commit/결과 해석만 남기고 스트림 읽기·대기는 `Transfers.kt`로 이동 (또는 Kotlin으로 변환 — 구현자 재량), `Dispatch.kt`
- Test: smoke `install`, `install_truncated`, `pull_push`, `pm_*`

**Interfaces:**
- Produces:
  - `class StatusReceiver { val intentSender: IntentSender; suspend fun await(timeoutMs: Long): Intent? }` — `IIntentSender` 프록시 콜백에서 `CompletableDeferred.complete`, 대기는 `withTimeoutOrNull`. 타임아웃 값은 기존 그대로(install 180_000 / install_multi 120_000 / uninstall 60_000).
  - `suspend fun Conn.copyChunksTo(out: OutputStream): Long` — 기존 `Files.copyChunks` 와이어 형식(`[len][chunk]…[len=0]`) 그대로 읽어 `withContext(Work){ out.write }`.
  - `suspend fun Conn.streamFileFrom(input: InputStream)` — `pull`의 청크 + 종결자 송신, 읽기는 Work.
  - `suspend fun install(c: Conn, size: Long, reinstall: Boolean, grant: Boolean): ByteArray`, `installMulti(c, sizes: LongArray, …)`, `push(c, path, mode, size)`, `pull(c, path)` — 응답 바이트는 기존과 동일. 업로드 중 EOF → 세션 `abandon()` 후 예외 전파(연결 종료).
- [ ] **Step 1:** 기존 `Installer.install/installMulti`, `Files.push/pull/drain` 의 응답 문자열·에러 태그를 목록으로 뽑아 `Transfers.kt` 주석이 아닌 smoke 케이스 기대값에 반영되었는지 확인 (`install_truncated` 포함).
- [ ] **Step 2:** 구현, Legacy 경로 제거.
- [ ] **Step 3:** 기기 패리티 (`smoke-t6.json`) — diff exit 0, `install_truncated` 후 `ping` 정상, `pm list sessions`에 남은 세션 없음.
- [ ] **Step 4: Commit** — `daemon: suspend-based install, pm and file transfer`

### Task 7: 서브프로세스 스트림 (dumpsys / logcat / shell / monitor / clip_watch)

**Files:**
- Create: `ProcStream.kt`
- Modify: `Dumpsys.java`, `Logcat.java`, `ShellExec.java`, `Lifecycle.java` → argv 구성만 남김(또는 Kotlin 변환), `Clipboard.java`(`watch` 제거, `currentText()` package-private), `Dispatch.kt`
- Test: smoke `logcat_head dumpsys_window shell_echo clip_watch_n disconnect_cleanup`

**Interfaces:**
- Produces:
  - `suspend fun runProcStream(c: Conn, argv: List<String>, exitTrailer: Boolean)` — `ProcessBuilder(argv).redirectErrorStream(true).start()`; stdout fd 획득(`FilterInputStream.in` 리플렉션 → `FileInputStream.getFD()`), `Os.fcntlInt(fd, F_SETFL, O_NONBLOCK)` 후 `IoLoop.watch(EVENT_INPUT)`로 `Os.read` → 읽은 만큼 `c.send(chunk)`(청크 크기 상한은 기존 코드의 버퍼 크기와 동일). EOF → `exitTrailer`이면 `__exit__ N` 프레임(기존 형식) → `len=0`. 연결 job 취소 시 `process.destroy()`. fd 획득이 불가능하면 Work에서 블로킹 read 후 main으로 전달하는 폴백 (spec 4.2 허용).
  - `clip_watch`: `Loop.scope` 위 `while(isActive){ delay(intervalMs); val t = withContext(Work){ clip.currentText() }; if (t != last) c.sendLatest(0, …) }` — 프레임 형식은 기존 `Clipboard.emit`과 동일.
- [ ] **Step 1:** 구현.
- [ ] **Step 2:** 기기 패리티 (`smoke-t7.json`) — diff exit 0, `disconnect_cleanup`(shell `sleep 100` 중 끊기 → 기기에서 해당 `sleep` 프로세스 사라짐) true.
- [ ] **Step 3: Commit** — `daemon: subprocess streams via looper fd events`

### Task 8: 화면 스트림 (`stream`, `stream_tilejpeg`, `stream_h264`) + 스크린샷 미러

**Files:**
- Create: `Streams.kt`
- Delete: `Streamer.java`
- Modify: `Screenshot.java`(미러별 `HandlerThread` 제거, `ImageReader` 리스너를 main `Handler`로; `FrameListener` 호출은 main), `TileStreamer.java`(패킷 생성 함수만 남김: `buildPacket(forceKeyframe): ByteArray`), `H264Streamer.java`(인코더 생성/설정·`requestKeyframe`만 남기고 드레인 루프 제거), `Dispatch.kt`
- Test: smoke `stream_n stream_tile_n stream_h264_n screenshot_* slow_client disconnect_cleanup`

**Interfaces:**
- Produces:
  - `suspend fun jpegStream(c: Conn, mirror: Screenshot.Mirror, quality: Int, fps: Int)` — 프레임 이벤트 → `Channel<Unit>(CONFLATED)`; fps>0이면 간격 유지, 0이면 `IDLE_INTERVAL_MS`(500) 재전송 동작 유지; 인코딩 `withContext(Work)`; 전송 `c.sendLatest(0, jpeg)`.
  - `suspend fun tileStream(c: Conn, mirror, quality, tile)` — 같은 구조, 패킷 생성은 Work, keyframe 강제 조건 기존과 동일, 전송은 `sendLatest`가 아닌 `send`(타일 델타는 누락 불가 — 기존 코드가 delta를 이전 프레임 기준으로 만든다면 무손실이어야 함; 기존 코드를 읽고 무손실 여부 확인 후 결정, 커밋 메시지에 기록).
  - `suspend fun h264Stream(c: Conn, args)` — `MediaCodec.setCallback(cb, mainHandler)`; `onOutputBufferAvailable`에서 버퍼를 `ByteArray`로 복사(복사는 작으므로 main 허용, 1MB 초과 프레임은 Work) 후 연결별 `Channel<ByteArray>(capacity = 64)`에 `send`(가득 차면 해당 연결의 드레인만 지연 — `releaseOutputBuffer`를 send 이후 호출). 송신 코루틴이 `c.send`. `keyframe` verb는 활성 세션 목록(main 전용 `mutableSetOf`)에 대해 `requestKeyframe()`, 응답 `ok n=<세션 수>`.
  - 기존 `H264Streamer`의 SPS/PPS·에러 프레임 형식·해상도 규칙(roundEven, 2048 cap) 그대로.
- [ ] **Step 1:** 구현.
- [ ] **Step 2:** 기기 패리티 (`smoke-t8.json`) — diff exit 0, `slow_client` true, `disconnect_cleanup` 후 `dumpsys display | grep -c hs-` 가 접속 전과 동일(VirtualDisplay 해제).
- [ ] **Step 3:** 수동: `handsets-viewer` 또는 `hs` TUI 미러링이 정상 표시(가능한 범위에서; 불가능하면 보고).
- [ ] **Step 4: Commit** — `daemon: screen streams on looper with work-pool encoding`

### Task 9: 정리 — Legacy 제거, 스레드·지연 검증, 문서

**Files:**
- Delete: `LegacyStreams.kt`, `Loop.Legacy`
- Modify: 남은 `synchronized`/`volatile`/`CopyOnWriteArrayList` 중 이제 main 전용인 것 제거(`Dumper`, `Input`, `Providers`, `Clipboard`, `Screenshot`, `State` 등 — 풀에서 접근하는 객체는 유지), `docs/architecture.md`(스레드 모델 절 추가, Kotlin 파일 반영), `README.md`
- Test: smoke 전체, `threads`, `bench`

- [ ] **Step 1:** `grep -rn "Legacy\|blockingIn\|blockingOut" src/` → 0건이 되도록 남은 경로 포팅.
- [ ] **Step 2:** `grep -rnE "new Thread|HandlerThread|Thread\.sleep|\.wait\(|BlockingQueue" src/` → 허용 목록만 남는지 확인: `Main.kt`의 UA 폴백(`hs-ua`, 사용 시), 셧다운 훅, `connectWithRetry`의 sleep(루퍼 시작 전·Work 스레드).
- [ ] **Step 3:** Run: `./gradlew test hsJar` → PASS. jar 크기 기준 대비 기록.
- [ ] **Step 4:** 최종 기기 검증: `run` → `smoke-final.json`, `diff build/smoke-a.json build/smoke-final.json` exit 0; `threads` 결과가 main(`hsd`) + `hs-work-*`(≤4) (+ `hs-ua` 폴백 시) + 프레임워크 스레드(`Binder:*`, `HeapTaskDaemon`, `ReferenceQueueD`, `FinalizerDaemon`, `FinalizerWatchd`, `Jit thread pool`, `Signal Catcher`, `Profile Saver`, `perfetto_hprof_`, `ADB-JDWP`, MediaCodec 관련) 만; `bench` p50/p95가 `build/bench-baseline.txt` 대비 +10% 이내.
- [ ] **Step 5: Commit** — `daemon: drop legacy bridges; document looper thread model`

### Task 10 (선택, 여유 시): 스트림 공유

**Files:**
- Modify: `Streams.kt`, `H264Streamer.java`(또는 Kotlin화), `Screenshot.java`
- Test: smoke에 `stream_h264_shared`(같은 파라미터 2개 연결 + 다른 파라미터 1개 연결) 케이스 추가

**Interfaces:**
- Produces: `class SharedH264(key: H264Key) { fun join(c: Conn): Job; }` 레지스트리 `Map<H264Key, SharedH264>` (main 전용), `data class H264Key(w: Int, h: Int, fps: Int, kbps: Int, gopSec: Int)` — 정규화된 값 기준. 인코더 출력은 한 번 복사한 `ByteArray`를 모든 구독자 채널에 같은 참조로 전달. 합류 시 캐시한 codec-config 먼저 → IDR 요청 → 다음 IDR부터 전달. 구독자 채널(capacity 64) `trySend` 실패 시 그 구독자 큐 비우고 `waitingIdr = true` + IDR 요청. 마지막 구독자 이탈 시 인코더/VirtualDisplay 해제. `keyframe` 응답 `n` = 연결 수. JPEG/tile: `(mirror, q[, tile])` 키로 프레임당 인코딩 1회 공유 (tile은 연결별 이전 프레임 상태가 있으면 공유 대상에서 제외 — 구현자가 코드 확인 후 결정).
- [ ] **Step 1:** 구현.
- [ ] **Step 2:** smoke: `stream_h264_shared`에서 두 공유 연결이 모두 첫 프레임 전 SPS/PPS, 첫 비디오 프레임이 IDR, `dumpsys media.codec`(또는 `threads`)로 인코더 2개(공유 1 + 별도 1) 확인. 기존 케이스 diff exit 0.
- [ ] **Step 3: Commit** — `daemon: share encoders across identical stream subscribers`
