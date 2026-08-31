# R8 rules for the release build.
#
# Almost everything here exists because of one thing: UniFFI reaches the Rust
# library through JNA, and JNA is entirely reflective. R8 cannot see that a
# class is instantiated from native code, so without these rules it removes or
# renames types that are only ever named by a string at runtime, and the app
# fails at the first FFI call with a NoSuchMethodError or a
# ClassNotFoundException -- in release only, and not until the code path runs.

# --- JNA -------------------------------------------------------------------
# JNA maps Java types onto the C ABI by reflection over field names and order.
# Renaming a field silently changes the memory layout the native side reads.
-keep class com.sun.jna.** { *; }
-keep class * implements com.sun.jna.** { *; }
-keepclassmembers class * extends com.sun.jna.Structure {
    <fields>;
}
-dontwarn java.awt.*

# --- UniFFI bindings -------------------------------------------------------
# The generated bindings define JNA structures and callback interfaces the
# Rust side invokes by name. `uniffi.beam_client_core` is generated wholesale,
# so it is kept wholesale rather than enumerating types a regeneration would
# change.
-keep class uniffi.beam_client_core.** { *; }
-keep interface uniffi.beam_client_core.** { *; }

# The core calls back into Kotlin for storage. A callback the Rust side invokes
# reflectively is, to R8, an interface nothing calls.
-keep class dev.beam.android.core.ffi.storage.** { *; }

# --- Kotlin serialization --------------------------------------------------
# Serializers are resolved from the companion object at runtime.
-keepclassmembers @kotlinx.serialization.Serializable class ** {
    *** Companion;
    *** serializer(...);
}
-keepclasseswithmembers class ** {
    kotlinx.serialization.KSerializer serializer(...);
}

# Navigation 3 destinations are @Serializable and are also looked up by type
# when the back stack is restored.
-keep class dev.beam.android.navigation.** { *; }

# --- Media3 ----------------------------------------------------------------
# Media3 ships consumer rules for its own reflection, but the renderer and
# extension classes it instantiates by name are not covered by them.
-dontwarn androidx.media3.**
-keep class androidx.media3.exoplayer.** { *; }

# --- Coroutines ------------------------------------------------------------
-dontwarn kotlinx.coroutines.**
-keepclassmembers class kotlinx.coroutines.** {
    volatile <fields>;
}

# --- Diagnostics -----------------------------------------------------------
# Keeps line numbers in a release stack trace while still obfuscating names.
# Without this a crash report from a release build is unreadable.
-keepattributes SourceFile,LineNumberTable
-renamesourcefileattribute SourceFile
