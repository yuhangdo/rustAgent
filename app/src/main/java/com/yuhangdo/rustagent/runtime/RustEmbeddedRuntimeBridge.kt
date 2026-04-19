package com.yuhangdo.rustagent.runtime

interface NativeRustRuntimeBridge {
    fun ensureServerStarted(appStorageDir: String): Int
}

object RustEmbeddedRuntimeBridge : NativeRustRuntimeBridge {
    private const val LIB_NAME = "claude_code_rs"

    @Volatile
    private var loadAttempted = false

    @Volatile
    private var loadFailure: Throwable? = null

    override fun ensureServerStarted(appStorageDir: String): Int {
        ensureLibraryLoaded()
        return nativeEnsureServerStarted(appStorageDir)
    }

    private fun ensureLibraryLoaded() {
        if (loadAttempted) {
            loadFailure?.let { throw IllegalStateException(it.message, it) }
            return
        }

        synchronized(this) {
            if (loadAttempted) {
                loadFailure?.let { throw IllegalStateException(it.message, it) }
                return
            }

            try {
                System.loadLibrary(LIB_NAME)
            } catch (throwable: Throwable) {
                loadFailure = throwable
                throw IllegalStateException(
                    "Embedded Rust runtime native library is missing. Build native/claude-code-rust with the mobile-bridge feature first.",
                    throwable,
                )
            } finally {
                loadAttempted = true
            }
        }
    }

    @JvmStatic
    private external fun nativeEnsureServerStarted(appStorageDir: String): Int
}
