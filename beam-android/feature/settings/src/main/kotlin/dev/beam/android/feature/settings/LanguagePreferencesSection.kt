package dev.beam.android.feature.settings

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.ListItem
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.ImeAction
import uniffi.beam_client_core.LanguagePreferences
import uniffi.beam_client_core.SubtitleMode

/**
 * The viewer's audio and subtitle languages and subtitle mode (issue #329),
 * which the server reads to name the tracks playback starts on. Stored on the
 * server rather than on this device, so every device starts on the same ones.
 */
internal fun LazyListScope.languagePreferenceItems(
    languages: LanguagePreferences,
    onChange: (LanguagePreferences) -> Unit,
) {
    item(key = "audio-languages") {
        LanguageListRow(
            title = "Audio languages",
            languages = languages.audioLanguages,
            whenEmpty = "The file's own default",
            onChange = { onChange(languages.copy(audioLanguages = it)) },
        )
    }
    item(key = "subtitle-mode") {
        // Cycles, as every three-way choice on this screen does.
        ListItem(
            headlineContent = { Text("Subtitles") },
            supportingContent = { Text(languages.subtitleMode.label()) },
            modifier =
                Modifier
                    .fillMaxWidth()
                    .clickable {
                        val modes = SubtitleMode.entries
                        val next = modes[(modes.indexOf(languages.subtitleMode) + 1) % modes.size]
                        onChange(languages.copy(subtitleMode = next))
                    },
        )
    }
    item(key = "subtitle-languages") {
        LanguageListRow(
            title = "Subtitle languages",
            languages = languages.subtitleLanguages,
            whenEmpty = "The file's own default",
            onChange = { onChange(languages.copy(subtitleLanguages = it)) },
        )
    }
}

internal fun SubtitleMode.label(): String =
    when (this) {
        SubtitleMode.OFF -> "Off"
        SubtitleMode.FORCED_ONLY -> "Only for foreign dialogue"
        SubtitleMode.ON -> "Always on"
    }

/**
 * The languages typed into the editor, most preferred first: separated by
 * commas, each trimmed, blanks dropped. What each names is the server's to
 * decide, so nothing is checked here.
 */
internal fun parseLanguages(text: String): List<String> = text.split(',').map { it.trim() }.filter { it.isNotEmpty() }

@Composable
private fun LanguageListRow(
    title: String,
    languages: List<String>,
    whenEmpty: String,
    onChange: (List<String>) -> Unit,
) {
    var editing by remember { mutableStateOf(false) }
    ListItem(
        headlineContent = { Text(title) },
        supportingContent = { Text(if (languages.isEmpty()) whenEmpty else languages.joinToString(", ")) },
        modifier =
            Modifier
                .fillMaxWidth()
                .clickable { editing = true },
    )
    if (editing) {
        var text by remember { mutableStateOf(languages.joinToString(", ")) }
        AlertDialog(
            onDismissRequest = { editing = false },
            title = { Text(title) },
            text = {
                OutlinedTextField(
                    value = text,
                    onValueChange = { text = it },
                    label = { Text("Most preferred first") },
                    supportingText = { Text("Codes or names, separated by commas: ja, English") },
                    singleLine = true,
                    keyboardOptions = KeyboardOptions(imeAction = ImeAction.Done),
                )
            },
            confirmButton = {
                TextButton(
                    onClick = {
                        editing = false
                        onChange(parseLanguages(text))
                    },
                ) { Text("Save") }
            },
            dismissButton = { TextButton(onClick = { editing = false }) { Text("Cancel") } },
        )
    }
}
