package dev.beam.android.feature.admin

import dev.beam.android.core.model.LoadState
import dev.beam.android.core.model.valueOrNull
import dev.beam.android.core.testing.FakeAdminRepository
import dev.beam.android.core.testing.FakeCatalogRepository
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import uniffi.beam_client_core.BeamException

@OptIn(ExperimentalCoroutinesApi::class)
class AdminTest {
    private val dispatcher = StandardTestDispatcher()

    @Before
    fun setUp() = Dispatchers.setMain(dispatcher)

    @After
    fun tearDown() = Dispatchers.resetMain()

    @Test
    fun `the dashboard loads`() =
        runTest {
            val viewModel = AdminViewModel(FakeAdminRepository(), FakeCatalogRepository())
            testScheduler.advanceUntilIdle()

            val state = viewModel.state.value.valueOrNull
            assertNotNull(state)
            assertTrue(state!!.libraries.isNotEmpty())
        }

    @Test
    fun `a non-administrator sees the forbidden failure rather than an empty dashboard`() =
        runTest {
            // The server is the control, not the UI. Rendering an empty
            // dashboard would suggest the server had nothing on it.
            val admin =
                FakeAdminRepository().apply {
                    failWith = BeamException.Forbidden("admin only")
                }
            val viewModel = AdminViewModel(admin, FakeCatalogRepository())
            testScheduler.advanceUntilIdle()

            val state = viewModel.state.value
            assertTrue(state is LoadState.Failure)
            assertFalse(
                "no retry for a permission the account does not have",
                (state as LoadState.Failure).retryable,
            )
        }

    @Test
    fun `a scan reports how many files it added`() =
        runTest {
            val admin = FakeAdminRepository()
            val viewModel = AdminViewModel(admin, FakeCatalogRepository())
            testScheduler.advanceUntilIdle()
            val library =
                viewModel.state.value.valueOrNull!!
                    .libraries
                    .first()

            viewModel.scan(library.id)
            testScheduler.advanceUntilIdle()

            val message =
                viewModel.state.value.valueOrNull!!
                    .message
            assertNotNull(message)
            assertTrue(message!!.contains("files added"))
        }

    @Test
    fun `a scan clears its in-progress marker even when it fails`() =
        runTest {
            // A stuck spinner beside a library is indistinguishable from a scan
            // that never finishes, and the operator has no way to clear it.
            val admin = FakeAdminRepository()
            val viewModel = AdminViewModel(admin, FakeCatalogRepository())
            testScheduler.advanceUntilIdle()
            val library =
                viewModel.state.value.valueOrNull!!
                    .libraries
                    .first()

            admin.failWith = BeamException.Server(500u, "scan failed")
            viewModel.scan(library.id)
            testScheduler.advanceUntilIdle()

            assertNull(
                viewModel.state.value.valueOrNull
                    ?.scanningLibraryId,
            )
        }

    @Test
    fun `blocking an account is reflected after the reload`() =
        runTest {
            val admin = FakeAdminRepository()
            val viewModel = AdminViewModel(admin, FakeCatalogRepository())
            testScheduler.advanceUntilIdle()
            val user =
                viewModel.state.value.valueOrNull!!
                    .users
                    .first()

            viewModel.setUserDisabled(user.id, disabled = true)
            testScheduler.advanceUntilIdle()

            assertTrue(
                viewModel.state.value.valueOrNull!!
                    .users
                    .first { it.id == user.id }
                    .disabled,
            )
        }

    @Test
    fun `a user listing that fails does not take the dashboard with it`() =
        runTest {
            // The counts and the libraries are the point of the screen; the user
            // list is one section of it.
            val admin = FakeAdminRepository().apply { usersFailWith = BeamException.Forbidden("no") }
            val viewModel = AdminViewModel(admin, FakeCatalogRepository())
            testScheduler.advanceUntilIdle()

            val state = viewModel.state.value.valueOrNull
            assertNotNull(state)
            assertTrue(state!!.users.isEmpty())
            assertTrue(state.libraries.isNotEmpty())
        }

    @Test
    fun `dismissing a message clears it`() =
        runTest {
            val viewModel = AdminViewModel(FakeAdminRepository(), FakeCatalogRepository())
            testScheduler.advanceUntilIdle()
            val library =
                viewModel.state.value.valueOrNull!!
                    .libraries
                    .first()

            viewModel.scan(library.id)
            testScheduler.advanceUntilIdle()
            viewModel.clearMessage()

            assertNull(
                viewModel.state.value.valueOrNull
                    ?.message,
            )
        }
}
