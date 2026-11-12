package dev.beam.android.di

import dagger.Module
import dagger.Provides
import dagger.hilt.components.SingletonComponent
import dagger.hilt.testing.TestInstallIn
import dev.beam.android.core.ffi.di.FfiBindsModule
import dev.beam.android.core.ffi.di.FfiProvidesModule
import dev.beam.android.core.ffi.preferences.PreferencesRepository
import dev.beam.android.core.ffi.repository.AdminRepository
import dev.beam.android.core.ffi.repository.CatalogRepository
import dev.beam.android.core.ffi.repository.PlaybackRepository
import dev.beam.android.core.ffi.repository.ServerRepository
import dev.beam.android.core.ffi.repository.SessionRepository
import dev.beam.android.core.ffi.repository.TelemetryRepository
import dev.beam.android.core.testing.FakeAdminRepository
import dev.beam.android.core.testing.FakeCatalogRepository
import dev.beam.android.core.testing.FakePlaybackRepository
import dev.beam.android.core.testing.FakePreferencesRepository
import dev.beam.android.core.testing.FakeServerRepository
import dev.beam.android.core.testing.FakeSessionRepository
import dev.beam.android.core.testing.FakeTelemetryRepository
import dev.beam.android.core.testing.Fixtures
import uniffi.beam_client_core.BeamClient
import javax.inject.Singleton

/**
 * The core, replaced by the `:core:testing` fakes for every Hilt test in the
 * app.
 *
 * Both of `:core:ffi`'s modules go: the real repositories sit over a
 * `BeamClient` that talks to a server, which a test has none of. (Robolectric
 * can load the JNA binding -- the feature modules' suites do, through
 * `beam.android.nativecore` -- so the native library is not the reason.) Each fake
 * is provided as itself as well as behind its interface, so a test injects the
 * same instance the screens read and drives it directly.
 *
 * The device starts signed in to one server, which is what a stored session
 * restores as. A test that needs another start configures the fakes before it
 * launches the activity.
 */
@Module
@TestInstallIn(
    components = [SingletonComponent::class],
    replaces = [FfiProvidesModule::class, FfiBindsModule::class],
)
object FakeCoreModule {
    /**
     * The core itself, which no test here may reach.
     *
     * Still bound because the graph names it: `BeamApplication` injects the
     * artwork loader built over it, and Hilt validates the application's
     * injector even though `HiltTestApplication` replaces the application
     * and nothing calls it. Anything else that asks for the core fails here,
     * loudly, rather than on JNA's missing library.
     */
    @Provides
    fun beamClient(): BeamClient = throw UnsupportedOperationException("the native core is not loaded under test")

    @Provides
    @Singleton
    fun fakeServers(): FakeServerRepository = FakeServerRepository(listOf(Fixtures.server(isActive = true)))

    @Provides
    fun servers(fake: FakeServerRepository): ServerRepository = fake

    @Provides
    @Singleton
    fun fakeCatalog(): FakeCatalogRepository = FakeCatalogRepository()

    @Provides
    fun catalog(fake: FakeCatalogRepository): CatalogRepository = fake

    @Provides
    @Singleton
    fun fakePlayback(): FakePlaybackRepository = FakePlaybackRepository()

    @Provides
    fun playback(fake: FakePlaybackRepository): PlaybackRepository = fake

    @Provides
    @Singleton
    fun fakeSessions(): FakeSessionRepository = FakeSessionRepository()

    @Provides
    fun sessions(fake: FakeSessionRepository): SessionRepository = fake

    @Provides
    @Singleton
    fun fakeAdmin(catalog: FakeCatalogRepository): FakeAdminRepository = FakeAdminRepository(catalog)

    @Provides
    fun admin(fake: FakeAdminRepository): AdminRepository = fake

    @Provides
    @Singleton
    fun fakeTelemetry(): FakeTelemetryRepository = FakeTelemetryRepository()

    @Provides
    fun telemetry(fake: FakeTelemetryRepository): TelemetryRepository = fake

    @Provides
    @Singleton
    fun fakePreferences(): FakePreferencesRepository = FakePreferencesRepository()

    @Provides
    fun preferences(fake: FakePreferencesRepository): PreferencesRepository = fake
}
