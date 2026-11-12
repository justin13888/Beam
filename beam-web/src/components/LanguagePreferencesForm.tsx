import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { RefreshCw } from "lucide-react";
import { useState } from "react";
import { toast } from "sonner";
import type { components } from "@/api.gen";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { apiClient } from "@/lib/apiClient";
import { apiError } from "@/lib/problem";

type LanguagePreferenceSettings =
	components["schemas"]["LanguagePreferenceSettings"];
type SubtitleMode = components["schemas"]["SubtitleMode"];

const LANGUAGE_PREFERENCES_KEY = ["language-preferences"] as const;

const SUBTITLE_MODES: { value: SubtitleMode; label: string }[] = [
	{ value: "off", label: "Off" },
	{ value: "forced_only", label: "Only for foreign dialogue" },
	{ value: "on", label: "Always on" },
];

/**
 * The languages typed into a field, most preferred first: separated by
 * commas, each trimmed, blanks dropped. What each names is the server's to
 * decide, so nothing is checked here.
 */
export function parseLanguages(text: string): string[] {
	return text
		.split(",")
		.map((language) => language.trim())
		.filter((language) => language.length > 0);
}

/**
 * The viewer's audio and subtitle languages and subtitle mode (issue #329),
 * which the server reads to name the tracks playback starts on. Stored on the
 * server, so every device starts on the same ones.
 */
export function LanguagePreferencesForm() {
	const { data, error, isLoading, refetch } = useQuery({
		queryKey: LANGUAGE_PREFERENCES_KEY,
		queryFn: async (): Promise<LanguagePreferenceSettings> => {
			const { data, error } = await apiClient.GET(
				"/v1/me/language-preferences",
				{ credentials: "include" },
			);
			if (error) throw apiError(error, "Failed to load language preferences");
			return data;
		},
	});

	if (isLoading) {
		return (
			<div className="flex items-center gap-3 py-4 text-gray-400">
				<RefreshCw className="animate-spin" size={18} />
				<span>Loading language preferences...</span>
			</div>
		);
	}
	if (error || !data) {
		return (
			<div className="space-y-4 py-4">
				<p className="text-red-400">
					Error: {error?.message ?? "No language preferences"}
				</p>
				<Button
					onClick={() => refetch()}
					variant="outline"
					className="border-gray-600 text-gray-300 hover:bg-gray-700"
				>
					Retry
				</Button>
			</div>
		);
	}
	// Keyed by what the server stored, so a save the server normalised (`ja`
	// stored as `jpn`) shows its answer rather than what was typed.
	return <LanguageFields key={JSON.stringify(data)} stored={data} />;
}

function LanguageFields({ stored }: { stored: LanguagePreferenceSettings }) {
	const queryClient = useQueryClient();
	const [audio, setAudio] = useState(stored.audio_languages.join(", "));
	const [subtitles, setSubtitles] = useState(
		stored.subtitle_languages.join(", "),
	);
	const [mode, setMode] = useState<SubtitleMode>(stored.subtitle_mode);

	const save = useMutation({
		mutationFn: async (): Promise<LanguagePreferenceSettings> => {
			const { data, error } = await apiClient.PUT(
				"/v1/me/language-preferences",
				{
					body: {
						audio_languages: parseLanguages(audio),
						subtitle_languages: parseLanguages(subtitles),
						subtitle_mode: mode,
					},
					credentials: "include",
				},
			);
			if (error) throw apiError(error, "Failed to save language preferences");
			return data;
		},
		onSuccess: async (saved) => {
			queryClient.setQueryData(LANGUAGE_PREFERENCES_KEY, saved);
			// Every title's sources name their default tracks from these, so a
			// list read before the change starts on the old ones.
			await queryClient.invalidateQueries({
				predicate: (query) =>
					query.queryKey[0] === "media" && query.queryKey[2] === "sources",
			});
			toast.success("Language preferences saved");
		},
	});

	return (
		<form
			className="space-y-5"
			onSubmit={(event) => {
				event.preventDefault();
				save.mutate();
			}}
		>
			<div className="space-y-2">
				<Label htmlFor="audio-languages" className="text-gray-300">
					Audio languages
				</Label>
				<Input
					id="audio-languages"
					value={audio}
					onChange={(event) => setAudio(event.target.value)}
					placeholder="The file's own default"
					className="border-gray-700 bg-gray-900 text-white"
				/>
			</div>
			<div className="space-y-2">
				<Label htmlFor="subtitle-mode" className="text-gray-300">
					Subtitles
				</Label>
				<select
					id="subtitle-mode"
					value={mode}
					onChange={(event) => setMode(event.target.value as SubtitleMode)}
					className="rounded-md border border-gray-700 bg-gray-900 px-3 py-1.5 text-sm text-white"
				>
					{SUBTITLE_MODES.map((option) => (
						<option key={option.value} value={option.value}>
							{option.label}
						</option>
					))}
				</select>
			</div>
			<div className="space-y-2">
				<Label htmlFor="subtitle-languages" className="text-gray-300">
					Subtitle languages
				</Label>
				<Input
					id="subtitle-languages"
					value={subtitles}
					onChange={(event) => setSubtitles(event.target.value)}
					placeholder="The file's own default"
					className="border-gray-700 bg-gray-900 text-white"
				/>
			</div>
			<p className="text-xs text-gray-500">
				Most preferred first, separated by commas: codes or names, such as
				&ldquo;ja, English&rdquo;.
			</p>
			{save.error && <p className="text-red-400">{save.error.message}</p>}
			<Button type="submit" disabled={save.isPending}>
				{save.isPending ? (
					<RefreshCw size={16} className="mr-2 animate-spin" />
				) : null}
				Save languages
			</Button>
		</form>
	);
}
