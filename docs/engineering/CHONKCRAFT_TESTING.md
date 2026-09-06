# Isolated real Chonkcraft input checks

The optional `chonkcraft` integration target runs an externally supplied game
jar with its licensed asset pack and JDK. It does not download or redistribute
game assets, change the game source, or read/write the player's saves.

```sh
game_scratch=$(mktemp -d /tmp/ccg.XXXXXX)
TMPDIR="$game_scratch" LIBGL_ALWAYS_SOFTWARE=1 GALLIUM_DRIVER=llvmpipe \
  CHONKSTEP_CHONKCRAFT_JAVA=/absolute/path/to/jdk/bin/java \
  CHONKSTEP_CHONKCRAFT_JAR=/absolute/path/to/game-app.jar \
  CHONKSTEP_CHONKCRAFT_PACK=/absolute/path/to/licensed.chonkpack \
  CHONKSTEP_WAYLAND_BIN=/absolute/path/to/preserved/chonkstep-wayland \
  CARGO_BUILD_JOBS=4 scripts/e2e.sh --headless --release --test chonkcraft --nocapture
```

Bubblewrap and a JDK with `javac` are required. Without the explicit Java
variable, the cases report SKIP and provide no game coverage. Once requested,
missing inputs, sandbox failures or missing observation hooks fail the test.

The game runs through the private session's announced XWayland server with
XToolkit selected explicitly. Its network, `/run`, `/tmp` and devices are
isolated; only the private X socket is exposed. JDK, jar and asset pack are
read-only. XDG directories, `user.home`, `chonkcraft.home` and CHONKCRAFT_HOME
all target a private writable game directory. Gamepad access is disabled;
there is no live desktop bus, display, GPU or audio-device access. A bounded
64–512 MiB Java heap is a fixture setting, not a recommended production limit.

`ChonkGameObserver.java` calls the actual game entry point before observing AWT,
so the game still chooses its normal Java2D pipeline before toolkit startup.
It logs delivered mouse/key events and reads painted menu geometry/captions.
It never uses Robot, dispatches synthetic AWT input, or calls a game action hook.
All actions enter through the compositor's private test door.

The matrix checks 1×, 1.5× and 2× output scales, real Campaign/Previous Menu
clicks while windowed and compositor-fullscreen, both button edges at the
expected AWT coordinates, and exact restoration of the windowed rectangle.
The graphics transform is observed, not assumed equal to output scale: this
JBR chooses a 2× AWT transform at both 1.5× and 2× output settings.

These are input/geometry checks. The observer, private software renderer and
concurrent campaign workloads make this unsuitable for FPS, input-latency,
native GPU or gaming-performance claims. In-battle selection, pointer capture,
held-key behavior and the game's own fullscreen control require separate cases.
