# Only the entry point keeps its name (the manifest and `am instrument` name
# it). Everything else, androidx.test included, moves under one package of
# the probe's own, so nothing it bundles can shadow a class of the target.
-keep class dev.handsets.probe.ProbeInstrumentation { <init>(); }
-repackageclasses 'dev.handsets.probe.lib'
-allowaccessmodification

# androidx.test refers to JUnit, Espresso and test-services classes it only
# uses when they are present.
-dontwarn **
