package dev.beam.android.feature.auth

import android.content.Context
import android.content.pm.PackageManager
import android.webkit.WebView
import dagger.Binds
import dagger.Module
import dagger.hilt.InstallIn
import dagger.hilt.android.qualifiers.ApplicationContext
import dagger.hilt.components.SingletonComponent
import javax.inject.Inject

/**
 * Whether this device has a browser a person can sign in with.
 *
 * It decides which sign-in [AuthViewModel] opens with (ADR-0017 D151-8): a
 * phone keeps the in-app browser and offers device sign-in as a second choice,
 * while a device without a usable browser goes straight to the code. An
 * interface so both paths can be tested without a device of each kind.
 */
public fun interface BrowserAvailability {
    /** `true` when an in-app browser sign-in is something a person can use here. */
    public fun hasUsableBrowser(): Boolean
}

/**
 * The platform's answer: no browser on a TV (`FEATURE_LEANBACK`), whose remote
 * makes a web form unusable even where a WebView exists, nor where no WebView
 * package is installed at all.
 */
internal class PlatformBrowserAvailability
    @Inject
    constructor(
        @ApplicationContext private val context: Context,
    ) : BrowserAvailability {
        override fun hasUsableBrowser(): Boolean =
            !context.packageManager.hasSystemFeature(PackageManager.FEATURE_LEANBACK) &&
                WebView.getCurrentWebViewPackage() != null
    }

@Module
@InstallIn(SingletonComponent::class)
internal abstract class AuthModule {
    @Binds
    abstract fun browserAvailability(platform: PlatformBrowserAvailability): BrowserAvailability
}
