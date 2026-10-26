# 데몬 단일 루퍼 리팩토링 + Gradle 빌드 전환 — 설계

날짜: 2026-10-25 · 대상: `src/dev/handsets/daemon/` (on-device daemon, `hs.jar`)

## 1. 의도와 이유

on-device 데몬은 지금 잘 동작하지만, 구조가 "필요할 때마다 스레드 하나"로 자라 왔다.

- `Server`: 블로킹 `accept()` + 연결당 `hs-conn` 스레드, 블로킹 `DataInputStream/DataOutputStream`, 곳곳의 `synchronized (out)`.
- `HandlerThread` 다수: `hs-ua`(UiAutomation), `State`의 `hs-state-pkg` / `hs-state-display` / `hs-state-cfg`, `Screenshot` 미러마다 `hs-mirror-listener-*`.
- `State` refresher 스레드(`wait/notify` + `Thread.sleep` 디바운스), `Main`의 warmup 스레드.
- `Dumpsys` / `Logcat` / `ShellExec` / `Lifecycle`의 서브프로세스 리더 스레드.
- `Streamer` / `TileStreamer` / `state_watch` 의 연결별 `BlockingQueue` 루프, `WaitRegistry`의 `lock.wait`, `LocalIntentSender.await` 블로킹.

사용자가 체감하는 문제는 없다. 동기는 **아키텍처적 불필요함의 제거**다: Android가 원래 제공하는 MainLooper + MessageQueue(epoll) 모델 하나로 이벤트를 모으고, 진짜 CPU 작업(버퍼/이미지 복사, 인코딩, 덤프 직렬화)만 작은 풀로 보낸다.

동시에, 지금은 IDE 전용인 Gradle 프로젝트를 **실제 `hs.jar`를 만드는 정식 빌드**로 만든다.

## 2. 목표 / 비목표 / 안티목표

**목표**
- 메인 스레드 하나가 `Looper.loop()`를 돌고, 모든 I/O·이벤트·타이머가 이 루퍼 위에서 처리된다.
- **루퍼는 절대 블로킹되지 않는다** (사용자 결정). 오래 기다리는 요청은 모두 suspend/콜백으로 바뀐다.
- CPU 작업만 작은 고정 크기 풀에서 수행한다.
- Kotlin + kotlinx-coroutines, `Dispatchers.Main` 기반 (사용자 결정).
- Gradle(`./gradlew`)이 javac/kotlinc → R8 → dex → `build/hs.jar`까지 만든다.

**비목표**
- 와이어 프로토콜 변경. `docs/wire.md`의 모든 verb, 인자, 응답, 프레이밍, 에러 문자열은 **바이트 단위로 유지** (사용자 결정). 호스트 쪽(`handsets-cli`, `handsets-tui`, `handsets-viewer`, `bindings/python`)은 수정하지 않는다.
- 새 기능 추가, 성능 튜닝(회귀가 없는 선에서).
- 모든 Java 파일의 Kotlin 변환.
- 외부 privileged-process 프레임워크 도입 (예: priv-kit). priv-kit은 설치된 앱(AAR, Compose UI)이 자기 권한 프로세스를 띄우는 Shizuku 계열 구조라, 앱 없이 호스트가 `app_process` jar를 직접 띄우는 이 데몬과 모델이 다르다. 의존성으로 쓰지 않고 필요하면 아이디어만 참고한다.

**안티목표 (동작해도 실패로 보는 것)**
- 메인 루퍼에서 binder 장시간 호출, 덤프, 인코딩, `Thread.sleep`, 블로킹 소켓 I/O가 일어나는 것.
- 한 클라이언트가 느리거나 멈췄을 때 다른 연결이 지연되는 것.
- 클라이언트 쪽에서 응답 차이가 관찰되는 것 (`hs` 명령 결과가 달라짐).
- `tap` 등 hot path 지연이 기존보다 눈에 띄게 나빠지는 것.
- `hs.jar`가 과도하게 커지는 것 (기준: 기존 대비 +300KB 이내를 목표, 초과 시 보고).

## 3. 제약

- 실행 환경: `CLASSPATH=/data/local/tmp/hs.jar app_process /system/bin dev.handsets.daemon.Main --port=N` (shell UID). 엔트리 `dev.handsets.daemon.Main.main(String[])` 시그니처·클래스명 유지.
- 산출물 위치 `build/hs.jar` 유지 (`handsets-cli/src/daemon.rs`가 이 경로를 찾고, `.github/workflows/release.yml`이 업로드).
- `--min-api 28` 유지. Kotlin JVM target은 R8/D8이 desugar 가능한 범위(11 또는 17)로.
- binder 스레드풀은 `app_process`가 만들므로 제거 대상이 아니다.
- SDK: `local.properties`의 `sdk.dir` (현재 `E:\packages\Android\Sdk`, platforms 36 / 37.0 / 37.2, build-tools 35 / 36).
- 작업 시작 시 워킹트리에 커밋되지 않은 Gradle 초안(`build.gradle.kts`, `settings.gradle.kts`, `gradle/`, `gradlew*`, `proguard-rules.pro`, `local.properties`)과 `build.sh` / `README.md` / `Screenshot.java` 수정이 있다. 이것을 출발점으로 삼는다.

## 4. 결정된 설계

### 4.1 스레드 모델

| 스레드 | 역할 |
|---|---|
| main (Looper) | 소켓 accept/read/write, 서브프로세스 stdout, a11y 이벤트, display/broadcast 리스너, `ImageReader` 콜백, 타이머, 모든 코루틴의 기본 실행지 |
| work pool (작은 고정 크기, 예: `min(4, cpu)`) | JPEG/WebP/PNG 인코딩, 비트맵/버퍼 복사, `dump`/`dump_active` 트리 순회+JSON, 길어질 수 있는 동기 binder 호출 |
| `MediaCodec` 내부 / binder 스레드 | 프레임워크 소유, 관여하지 않음 |

- `Main`: `prepareMainLooper()` → 초기화 → `Looper.loop()`. `hs-ua` HandlerThread 제거, `UiAutomation`은 main looper로 생성 — **먼저 시도** (사용자 결정).
  - 주의: `UiAutomation.connect()`는 서비스 연결 콜백(UA의 looper로 post됨)을 블로킹 대기한다. 따라서 main looper를 쓰려면 `Looper.loop()`가 돈 뒤에 connect를 **Work 스레드에서** 호출하고, connect 완료 후 UA 의존 핸들러들을 main에서 초기화한다. 서버 accept는 그 전에 열어도 되지만 UA가 필요한 verb는 준비될 때까지 suspend.
  - 기기(API 36)에서 이 방식이 안정적으로 동작하지 않으면 `hs-ua` HandlerThread 하나를 예외로 남기고, UA 이벤트 콜백은 즉시 main으로 post한다. 어느 쪽인지 결과를 보고한다.
- 코루틴: 루트 `CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)`. CPU 작업은 `withContext(Work)` (`Work` = 위 고정 풀을 `asCoroutineDispatcher()`로 감싼 것, 또는 `Dispatchers.Default.limitedParallelism(n)`).
- `Dispatchers.Main` 해석: kotlinx-coroutines-android의 `ServiceLoader` 경로가 `app_process`/dex-only jar에서 동작해야 한다. R8이 `META-INF/services/kotlinx.coroutines.internal.MainDispatcherFactory`를 입력으로 받아 치환하게 한다. **기기에서 확인 실패 시** `Handler(Looper.getMainLooper()).asCoroutineDispatcher()`로 대체하고 `Main`이라는 이름의 val로 노출한다 (내 판단).

### 4.2 이벤트 루프 I/O 계층 (신규, Kotlin)

- 서버 소켓: `ServerSocketChannel` non-blocking, 127.0.0.1, backlog 8, `SO_REUSEADDR`. fd를 `Looper.getMainLooper().queue.addOnFileDescriptorEventListener(fd, EVENT_INPUT, …)`로 등록해 accept.
  - fd 얻기: `ParcelFileDescriptor.fromSocket` / 리플렉션 등 구현자가 동작하는 방법을 고른다. 대안으로 `android.system.Os.socket/bind/listen/accept` + `FileDescriptor` 직접 사용도 허용 (구현자 재량).
- 연결(`Conn`): non-blocking 소켓 + 입력 버퍼 + 출력 큐.
  - `suspend fun readInt()`, `readFully(n)`, `readFrame()` — 버퍼가 부족하면 `EVENT_INPUT`을 기다리며 suspend.
  - `fun send(frame)` / `suspend fun sendAndDrain()` — 출력 큐에 넣고 `EVENT_OUTPUT`으로 흘림. 큐 상한을 넘으면 suspend (backpressure).
  - 이미지·상태 스트림(`stream`, `stream_tilejpeg`, `state_watch`, `clip_watch`)은 "최신 것만 유지" 정책: 이전 프레임이 아직 안 나갔으면 새 프레임으로 교체 (현재 `BlockingQueue(1)`/drop-oldest 동작과 동일한 의미).
  - `stream_h264` (1단계, 현재와 동일): 연결마다 인코더 하나, **프레임을 버리지 않는다** (P-frame 손실은 디코딩을 깨뜨림). 연결별 bounded 출력 큐 + backpressure로 그 연결의 인코더 드레인만 멈춘다.
  - 스트림 공유 (사용자 요청, **여유가 되면 마지막 단계**로): 같은 파라미터의 스트림 클라이언트들이 하나의 소스/인코더 출력을 공유한다.
    - `stream_h264`: 정규화한 파라미터(출력 크기, fps, bitrate, gop)가 같은 연결들은 인코더 + VirtualDisplay 하나를 공유. 인코더가 낸 access unit 버퍼를 한 번 복사해 불변 `ByteArray`로 만들고 모든 구독 연결에 같은 참조를 넣는다. 파라미터가 다르면 별도 인코더. 마지막 구독자가 나가면 해제.
    - 새 구독자 합류: 캐시해 둔 codec config(SPS/PPS)와 현재 연결이 기대하는 시작 프레임(현재 와이어가 첫 프레임 전에 보내는 것이 있다면 그것)을 먼저 보내고, 인코더에 IDR을 요청한 뒤 **다음 IDR부터** 전달.
    - 느린 구독자: 공유 인코더는 한 클라이언트 때문에 멈출 수 없으므로, 연결별 큐가 상한을 넘으면 그 연결의 대기 프레임을 버리고 다음 IDR까지 skip + IDR 요청 (해당 클라이언트만 잠깐 멈춘 듯 보이고 깨진 화면은 없음 — 내 판단).
    - `stream`/`stream_tilejpeg`도 같은 원칙: (size, q, tile)이 같으면 프레임당 인코딩 한 번, 결과 바이트를 공유.
    - `keyframe` verb는 활성 인코더 전부에 IDR 요청, 응답 `ok n=`의 n은 **연결 수**를 유지 (와이어 호환 — 내 판단).
  - `TCP_NODELAY` 유지.
  - EOF/에러/`EVENT_ERROR` → 연결 `Job` 취소 → 그 연결의 대기, 스트림, 서브프로세스, 구독이 전부 정리된다 (`try/finally`/`invokeOnCompletion`).
- 연결 하나 = 코루틴 하나. 그 안에서 기존 `Server.handle`과 같은 순서로 `readFrame → dispatch → respond`를 반복. 같은 연결에서 명령은 순차, 연결 간에는 독립.
- 서브프로세스(`dumpsys`, `logcat`, `shell`, `monitor`, `location`/`settings` 내부 호출 등): `Process` stdout fd를 같은 방식으로 등록해 읽고 청크 프레임으로 전달. 프로세스 종료 대기도 블로킹 `waitFor()` 대신 EOF 후 `exitValue()` 또는 풀에서 `waitFor`.
  - stdout fd 얻는 방법은 구현자 재량 (`ProcessBuilder` 대신 `Os.pipe` + `android.system.Os`/리플렉션 `ProcessImpl` 등). 깔끔한 방법이 없으면 **서브프로세스 읽기에 한해 풀 스레드에서 블로킹 read 후 main으로 post** 하는 것을 허용 (루퍼는 여전히 안 막힘).

### 4.3 블로킹 지점의 재작성

| 현재 | 변경 |
|---|---|
| `WaitRegistry.awaitIdle/awaitPredicate` (`lock.wait`) | main에서 a11y 이벤트 수신 → 대기자 목록 재평가. idle은 `postDelayed`/`delay` 타이머로, predicate 재평가는 이벤트마다. predicate가 노드 조회(binder)라면 `withContext(Work)`로. `suspend` API로 노출. |
| `LocalIntentSender.await(ms)` (`BlockingQueue`) — `Installer`, `Pm` | `CompletableDeferred<Intent>` + `withTimeoutOrNull`. |
| `install`/`install_multi`/`push`의 APK·파일 청크 수신 | `Conn.readFully` suspend로 읽고, 세션/파일 쓰기는 `Work`에서. |
| `State` refresher 스레드 | `markDirty()`가 main에서 debounce Job 재시작(`DEBOUNCE_MS`) + heartbeat는 `HEARTBEAT_MS` 반복 코루틴. `computeDevice()`는 `Work`에서, 결과 저장·구독자 fan-out은 main에서. |
| `State` pkg/display/cfg HandlerThread 3개 | 모두 main Handler에 등록. |
| `Screenshot` 미러별 HandlerThread | `ImageReader.setOnImageAvailableListener(…, mainHandler)`. 이미지 획득 직후 버퍼 복사·인코딩은 `Work`. |
| `Streamer` / `TileStreamer` / `H264Streamer` 연결 루프 | 프레임 이벤트 → `Channel(CONFLATED)` → 연결 코루틴이 `fps`/`IDLE_INTERVAL_MS` 타이밍으로 `Work`에서 인코딩 후 `Conn`으로 전송. H.264는 MediaCodec 출력 드레인을 콜백(`setCallback(…, mainHandler)`) 또는 `Work`로. |
| `state_watch`, `clip_watch` 큐 루프 | 구독 콜백 → conflated 채널 → 전송. |
| warmup 스레드 | `scope.launch(Work) { dumper.dumpActive() }`. |
| `Main.connectWithRetry`의 `Thread.sleep(250)` | 루퍼 시작 전 초기화 단계이므로 그대로 둬도 됨 (내 판단). |
| `Input`의 `swipe dur=`, `tap` 등 UiAutomation `injectInputEvent` | 이벤트 간 지연은 `delay()`. `injectInputEvent(sync=true)`가 길어질 수 있으면 `Work`에서 (구현자가 측정해 결정). |

- 루퍼에서 직접 실행해도 되는 동기 binder 호출의 기준: **짧고 상한이 있는 시스템 서비스 조회** (getprop, settings get, pm path 등). 그 외(덤프, provider query, 패키지 목록 등 수십 ms 이상 걸릴 수 있는 것)는 `Work`. 애매하면 `Work` (내 판단).
- 공유 상태는 main에서만 변경하는 것을 원칙으로 하고, 그 결과 불필요해진 `synchronized`/`volatile`/`CopyOnWriteArrayList`는 제거한다. 풀에서 만지는 객체(인코더 버퍼 등)는 해당 작업 안에서만 소유.

### 4.4 언어와 코드 배치

- Kotlin으로 새로 쓰는 것: `Main`(엔트리는 `@JvmStatic` 또는 `object Main`으로 `dev.handsets.daemon.Main.main` 유지), 이벤트 루프/`Conn`/서버, 디스패치(기존 `Server`의 verb 파싱·응답 문자열은 그대로 이식), `State`, `UiEvents`/`WaitRegistry`, 스트리머들, `Installer`/`Pm`/`LocalIntentSender`, 서브프로세스 스트리밍(`Dumpsys`/`Logcat`/`ShellExec`/`Lifecycle`).
- Java로 유지: 동기 헬퍼(`Props`, `Wm`, `Binders`, `Am`, `SettingsDirect`, `SettingsApi`, `Providers`, `Manifest`, `JsonOut`, `Traverse`, `Dumper`, `NodeActions`, `Clipboard`, `Notifications`, `Location`, `Files` 등). 내부의 `synchronized`는 호출 경로가 단일화된 뒤 필요 없으면 제거해도 되고 남겨도 무방.
- 소스 위치: `src/` 루트에 Java·Kotlin 공존 유지 (현재 Gradle 초안과 같음). 패키지 `dev.handsets.daemon`.

### 4.5 Gradle 빌드

- `build.gradle.kts`를 IDE 전용에서 정식 빌드로:
  - `kotlin("jvm")` + `java-library`, android.jar `compileOnly`/bootclasspath. `compileSdk`는 `-PandroidCompileSdk` 또는 설치된 최신 numeric 플랫폼 (현재 초안의 하드코딩 `37.2` 정리).
  - 의존성: `kotlin-stdlib`, `kotlinx-coroutines-core` + `kotlinx-coroutines-android` 최신 안정판 (현재 1.3.9 → 업그레이드).
  - 태스크 `hsJar`: 컴파일 클래스 + 런타임 의존성 jar → R8(`--release --min-api 28 --lib android.jar --pg-conf proguard-rules.pro`) → `classes.dex` → `build/hs.jar` (dex 하나만, 필요 시 `META-INF/services`는 R8 처리 후 불필요).
    - R8 실행 수단: **SDK의 R8** (사용자 결정). `.bat` 래퍼 대신 SDK `build-tools/<ver>/lib/d8.jar`(R8 메인 클래스 `com.android.tools.r8.R8` 포함)를 `JavaExec`로 실행 — Windows/macOS/Linux CI 공통 (내 판단). build-tools 버전은 `-PbuildTools=` 또는 설치된 최신; 기본 36.0.0.
  - `proguard-rules.pro`: 기존 `build.sh`의 r8.pro 내용(Main keep, `-repackageclasses ''`, `-allowaccessmodification`, `-dontwarn **`) + 코루틴 규칙.
  - `assemble`/`build`가 `hsJar`에 의존. Gradle 자체 출력은 지금 초안처럼 `build-gradle/`에 두되 최종 jar는 `build/hs.jar`로 복사.
- `build.sh`: `./gradlew hsJar` 호출하는 얇은 래퍼로 축소 (기존 사용자·문서 호환).
- CI `.github/workflows/release.yml`: JDK 셋업 + `./gradlew hsJar` (또는 래퍼 `build.sh` 그대로 호출). 산출물 경로 동일.
- `README.md`의 "Daemon IDE setup" 섹션과 `docs/architecture.md`의 `build.sh` 설명을 갱신.
- `local.properties`는 머신 로컬 파일이므로 git 추적에서 제외하는 것이 맞다 (현재 staged 상태 — 내 판단, 구현 시 unstage + `.gitignore`).

## 5. 검증

검증 방법은 사용자가 구현자에게 맡겼다. 결정:

1. **빌드**: `./gradlew hsJar` 성공, `build/hs.jar`에 `classes.dex`만, 크기 기록 (기존 `build.sh` 산출물과 비교).
2. **환경**: 기존 AVD `Medium_Phone`(API 36, x86_64, `E:\packages\Android\Sdk\emulator`) 하나만 기동. `min-api 28`은 유지하되 버전 매트릭스 검증은 하지 않는다 (사용자 결정: 상위 버전에서 현재 잘 동작하므로 충분). 호스트 CLI는 `handsets-cli`를 `cargo build --release`로 빌드해 사용 (`HANDSETS_JAR`로 jar 지정).
3. **와이어 스모크 테스트 스크립트** `scripts/wire-smoke.py` (신규): adb forward 후 소켓으로 `docs/wire.md`의 verb들을 직접 호출, 응답을 정규화(타임스탬프·elapsed·배터리 등 변동값 마스킹)해 저장. **기존 jar(현재 HEAD를 `build.sh`로 빌드)와 새 jar 결과를 diff**. 최소 커버리지:
   - 단발: `ping info dump dump_active screenshot(jpeg/png) tap swipe key text getprop settings_get wm_info pm_list pm_path state * clip_get/clip_set deeplinks sms/calls/contacts`
   - 대기: `wait_for_idle wait_for_text(성공/timeout) wait_for_activity tap_and_dump tap_and_settle`
   - 스트림: `stream`/`stream_h264`/`stream_tilejpeg` N프레임 수신 후 끊기, `state_watch`, `clip_watch`, `logcat`/`dumpsys`/`shell` 청크+종결자, `pull`/`push` 라운드트립, `install`(작은 테스트 APK)
   - 동시성: 스트림 하나를 일부러 읽지 않는 느린 클라이언트로 열어 둔 상태에서 다른 연결의 `ping`/`tap` 지연이 정상인지, 장시간 `wait_for_text timeout_ms=5000` 중 다른 연결 `ping`이 즉시 응답하는지.
   - 연결 정리: 스트림 중 클라이언트 강제 종료 후 데몬 스레드 수/구독자 수가 원복되는지.
4. **스레드 수 확인**: `adb shell ls /proc/<pid>/task` + `cat comm`로 기존/신규 비교, `hs-*` 스레드가 main + work pool만 남았는지.
5. **지연**: `scripts/bench-adb.sh`(또는 smoke 스크립트 내 `tap`/`dump_active`/`screenshot` 100회 p50/p95) 기존 대비 회귀 없음.
6. 수동 확인: `hs` TUI/뷰어 미러링이 정상 동작.

## 6. 구현자 재량으로 남긴 것

- fd 이벤트 등록에 쓸 소켓/파이프 fd 획득 방식 (4.2).
- Work 풀 크기와 구현 (`Executors` 기반 vs `limitedParallelism`).
- 어떤 동기 호출을 main에 둘지의 세부 판단 (4.3 기준 안에서).
- 파일/클래스 분할과 이름, Java 헬퍼 중 추가로 Kotlin화할 것.
- 작업 순서. 단, 매 단계에서 `hs.jar`가 빌드·동작 가능한 상태를 유지하는 점진적 순서를 권장 (예: Gradle 빌드 → 이벤트 루프/서버 → State/리스너 → 대기 → 스트림 → 서브프로세스 → 정리).
