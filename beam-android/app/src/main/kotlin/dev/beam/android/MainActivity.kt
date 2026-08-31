package dev.beam.android

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.viewModels
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.runtime.getValue
import androidx.core.splashscreen.SplashScreen.Companion.installSplashScreen
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import dagger.hilt.android.AndroidEntryPoint
import dev.beam.android.core.designsystem.BeamTheme
import dev.beam.android.core.model.PaletteSource
import dev.beam.android.core.model.ThemeMode
import dev.beam.android.ui.BeamApp

/** The only activity. */
@AndroidEntryPoint
public class MainActivity : ComponentActivity() {
    private val viewModel: MainViewModel by viewModels()

    override fun onCreate(savedInstanceState: Bundle?) {
        val splash = installSplashScreen()
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()

        // Held until the stored session has been read, so the app opens
        // straight onto the right screen instead of showing sign-in for a
        // frame and then replacing it.
        splash.setKeepOnScreenCondition { !viewModel.state.value.isReady }

        setContent {
            val state by viewModel.state.collectAsStateWithLifecycle()

            BeamTheme(
                darkTheme =
                    when (state.preferences.themeMode) {
                        ThemeMode.System -> isSystemInDarkTheme()
                        ThemeMode.Light -> false
                        ThemeMode.Dark -> true
                    },
                dynamicColor = state.preferences.paletteSource == PaletteSource.Dynamic,
            ) {
                BeamApp(
                    isSignedIn = state.isSignedIn,
                    isAdmin = state.isAdmin,
                )
            }
        }
    }
}
