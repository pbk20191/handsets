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
# kotlinx-coroutines: atomic field updaters 
-keepclassmembers class kotlinx.coroutines.** { volatile <fields>; }
-keepnames class kotlinx.coroutines.CoroutineExceptionHandler {}
