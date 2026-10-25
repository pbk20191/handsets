# Entry point: app_process invokes Main.main.
-keep public class dev.handsets.daemon.Main {
    public static void main(java.lang.String[]);
}
# Allow R8 to rename everything else and re-pack to a single root namespace.
-allowaccessmodification
-repackageclasses ''
# We never inspect class names at runtime (only framework class names via
# reflection, which R8 leaves alone), so renames are safe. Anonymous inner
# classes that implement framework interfaces (OnImageAvailableListener,
# FrameListener, Runnable) keep their override method names automatically.
-dontwarn **
# kotlinx-coroutines ships its own R8 rules (atomic updaters, debug paths), which R8 applies.
# Dispatchers.Main: ServiceLoader reads META-INF/services/kotlinx.coroutines.internal.MainDispatcherFactory
# (packed into hs.jar by the hsJar task) and instantiates the factory by name.
-keep class kotlinx.coroutines.android.AndroidDispatcherFactory { <init>(); }
-keepnames class kotlinx.coroutines.internal.MainDispatcherFactory

