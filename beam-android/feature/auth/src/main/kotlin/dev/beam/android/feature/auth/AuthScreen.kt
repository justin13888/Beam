package dev.beam.android.feature.auth

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.Close
import androidx.compose.material.icons.rounded.Dns
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import dev.beam.android.core.designsystem.BeamSpacing
import dev.beam.android.core.designsystem.component.SectionHeader

/** Sign-in, wired to its view model. */
@Composable
public fun AuthRoute(
    onSignedIn: () -> Unit,
    modifier: Modifier = Modifier,
    viewModel: AuthViewModel = hiltViewModel(),
) {
    val state by viewModel.state.collectAsStateWithLifecycle()

    AuthScreen(
        state = state,
        onAddressChange = viewModel::onAddressChange,
        onConnect = { viewModel.connect() },
        onConnectExisting = viewModel::connect,
        onForget = viewModel::forget,
        onAcceptCertificate = viewModel::acceptCertificate,
        onDeclineCertificate = viewModel::declineCertificate,
        onSessionCookie = viewModel::onSessionCookie,
        onSignInCancelled = viewModel::onSignInCancelled,
        onSignedIn = onSignedIn,
        modifier = modifier,
    )
}

/** Sign-in, as a function of its state. */
@Composable
internal fun AuthScreen(
    state: AuthUiState,
    onAddressChange: (String) -> Unit,
    onConnect: () -> Unit,
    onConnectExisting: (String) -> Unit,
    onForget: (String) -> Unit,
    onAcceptCertificate: (PendingTrust) -> Unit,
    onDeclineCertificate: () -> Unit,
    onSessionCookie: (String) -> Unit,
    onSignInCancelled: () -> Unit,
    onSignedIn: () -> Unit,
    modifier: Modifier = Modifier,
) {
    // A LaunchedEffect rather than a bare call: navigating from composition
    // fires again on every recomposition, and on configuration change it would
    // navigate a second time onto a destination already on the back stack.
    LaunchedEffect(state.isSignedIn) {
        if (state.isSignedIn) onSignedIn()
    }

    state.pendingTrust?.let { trust ->
        TrustPrompt(
            trust = trust,
            onAccept = { onAcceptCertificate(trust) },
            onDecline = onDeclineCertificate,
        )
    }

    val loginUrl = state.loginUrl
    if (loginUrl != null) {
        SignInWebView(
            url = loginUrl,
            onSessionCookie = onSessionCookie,
            modifier = modifier.fillMaxSize(),
        )
        return
    }

    Column(
        modifier =
            modifier
                .fillMaxSize()
                .imePadding()
                .padding(BeamSpacing.Large),
        verticalArrangement = Arrangement.spacedBy(BeamSpacing.Medium),
    ) {
        Text(
            text = "Connect to your Beam server",
            style = MaterialTheme.typography.headlineSmall,
        )
        Text(
            text = "Enter the address of the server you want to watch from.",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )

        OutlinedTextField(
            value = state.address,
            onValueChange = onAddressChange,
            label = { Text("Server address") },
            placeholder = { Text("beam.example.com") },
            singleLine = true,
            isError = state.error != null,
            supportingText = state.error?.let { { Text(it) } },
            leadingIcon = { Icon(Icons.Rounded.Dns, contentDescription = null) },
            keyboardOptions =
                KeyboardOptions(
                    // A URI keyboard rather than a text one: it puts `/`, `.` and
                    // `:` on the primary layer, which is most of what an address
                    // is, and suppresses autocorrect -- which otherwise cheerfully
                    // capitalises a hostname into one that does not resolve.
                    keyboardType = KeyboardType.Uri,
                    imeAction = ImeAction.Go,
                ),
            keyboardActions = KeyboardActions(onGo = { onConnect() }),
            modifier = Modifier.fillMaxWidth(),
        )

        Button(
            onClick = onConnect,
            enabled = state.canConnect,
            modifier = Modifier.fillMaxWidth(),
        ) {
            if (state.isConnecting) {
                CircularProgressIndicator(
                    modifier = Modifier.padding(end = BeamSpacing.Small),
                    strokeWidth = 2.dp,
                )
            }
            Text(if (state.isConnecting) "Connecting" else "Connect")
        }

        if (state.knownServers.isNotEmpty()) {
            SectionHeader(title = "Recent servers")
            LazyColumn(modifier = Modifier.fillMaxWidth()) {
                items(state.knownServers, key = { it.id }) { server ->
                    ListItem(
                        headlineContent = { Text(server.displayName) },
                        supportingContent = { Text(server.baseUrl) },
                        trailingContent = {
                            IconButton(onClick = { onForget(server.id) }) {
                                Icon(
                                    Icons.Rounded.Close,
                                    contentDescription = "Forget ${server.displayName}",
                                )
                            }
                        },
                        modifier =
                            Modifier
                                .fillMaxWidth()
                                .clickable { onConnectExisting(server.id) },
                    )
                }
            }
        }
    }
}
