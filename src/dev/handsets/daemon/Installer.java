package dev.handsets.daemon;

import android.content.Context;
import android.content.Intent;
import android.content.IntentSender;
import android.content.pm.PackageInstaller;
import android.content.pm.PackageManager;

import java.io.DataInputStream;
import java.io.IOException;
import java.io.OutputStream;
import java.lang.reflect.Method;
import java.nio.charset.StandardCharsets;

/**
 * Stream-install an APK via {@link PackageInstaller}, the same path
 * {@code adb install} uses internally. We skip the /data/local/tmp staging
 * copy by writing chunked frames straight into {@code session.openWrite}.
 *
 * Install result is reported back via a local {@link Binder} that implements
 * just the {@code IIntentSender.send} transaction (transaction code 1, stable
 * across all known API levels). That dodges the BroadcastReceiver dance that
 * would require a proper package name for our app_process daemon.
 */
final class Installer {

    private final Context ctx;

    Installer(Context ctx) { this.ctx = ctx; }

    static void trySetInstallFlags(PackageInstaller.SessionParams params, int flag) {
        try {
            Method m = PackageInstaller.SessionParams.class
                    .getDeclaredMethod("setInstallFlags", int.class);
            m.setAccessible(true);
            // setInstallFlags replaces; OR in our flag with whatever was there.
            java.lang.reflect.Field f =
                    PackageInstaller.SessionParams.class.getDeclaredField("installFlags");
            f.setAccessible(true);
            int prev = f.getInt(params);
            m.invoke(params, prev | flag);
        } catch (Throwable t) {
            // Fallback: write the field directly.
            try {
                java.lang.reflect.Field f =
                        PackageInstaller.SessionParams.class.getDeclaredField("installFlags");
                f.setAccessible(true);
                f.setInt(params, f.getInt(params) | flag);
            } catch (Throwable ignored) {}
        }
    }

    static void trySetRequireUserAction(PackageInstaller.SessionParams params, int mode) {
        try {
            Method m = PackageInstaller.SessionParams.class
                    .getDeclaredMethod("setRequireUserAction", int.class);
            m.invoke(params, mode);
        } catch (Throwable ignored) {
            // pre-API-31; default behaviour is fine.
        }
    }

    void grantAllRequested(String pkg) {
        try {
            PackageManager pm = ctx.getPackageManager();
            String[] requested = pm.getPackageInfo(pkg, PackageManager.GET_PERMISSIONS)
                    .requestedPermissions;
            if (requested == null) return;
            Object[] svcs = new Object[] {
                    Binders.iPermissionManagerOrNull(),
                    Binders.iPackageManager(),
            };
            Object svc = null;
            Method grant = null;
            for (Object s : svcs) {
                if (s == null) continue;
                Method m = findGrant(s.getClass());
                if (m != null) { svc = s; grant = m; break; }
            }
            if (grant == null) return;
            Class<?>[] gp = grant.getParameterTypes();
            int userId = Binders.myUserId();
            for (String perm : requested) {
                Object[] args = new Object[gp.length];
                args[0] = pkg; args[1] = perm;
                boolean uidSet = false;
                for (int i = 2; i < gp.length; i++) {
                    if (gp[i] == int.class && !uidSet) { args[i] = userId; uidSet = true; }
                    else if (gp[i] == int.class) args[i] = 0;
                    else if (gp[i] == boolean.class) args[i] = false;
                    else args[i] = null;
                }
                try { grant.invoke(svc, args); }
                catch (Throwable ignored) { /* non-runtime perms throw — that's fine */ }
            }
        } catch (Throwable t) {
            System.err.println("warn: grantAllRequested failed: " + t);
        }
    }

    private static Method findGrant(Class<?> cls) {
        Method picked = null;
        for (Method m : cls.getMethods()) {
            if (!"grantRuntimePermission".equals(m.getName())) continue;
            Class<?>[] p = m.getParameterTypes();
            if (p.length >= 3 && p[0] == String.class && p[1] == String.class
                    && (picked == null || p.length < picked.getParameterTypes().length)) {
                picked = m;
            }
        }
        return picked;
    }

    private static byte[] ok(String s) { return s.getBytes(StandardCharsets.UTF_8); }
    private static byte[] err(String tail) { return ("ERR:" + tail).getBytes(StandardCharsets.UTF_8); }
}
