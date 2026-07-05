import { useEffect, useState } from "react";
import type { components } from "@/api.gen";
import { env } from "@/env";

type AdminEventDto =
	components["schemas"]["beam_server.models.admin.AdminEventDto"];

const MAX_EVENTS = 50;

/** Live-tails `GET /v1/admin/events/stream` (SSE) so the admin dashboard
 * shows library scans and other administrative events as they happen,
 * instead of only ever seeing them after a manual refresh (FR-604).
 * `enabled` should gate this on the route already being admin-only --
 * there's no point holding an SSE connection open before that's true. */
export function useAdminEventStream(enabled: boolean) {
	const [events, setEvents] = useState<AdminEventDto[]>([]);
	const [connected, setConnected] = useState(false);

	useEffect(() => {
		if (!enabled) return;

		const source = new EventSource(
			`${env.C_STREAM_SERVER_URL}/v1/admin/events/stream`,
			{ withCredentials: true },
		);
		source.onopen = () => setConnected(true);
		source.onerror = () => setConnected(false);
		source.onmessage = (message) => {
			try {
				const event: AdminEventDto = JSON.parse(message.data);
				setEvents((prev) => [event, ...prev].slice(0, MAX_EVENTS));
			} catch (err) {
				console.error("Failed to parse admin event", err);
			}
		};

		return () => {
			source.close();
			setConnected(false);
		};
	}, [enabled]);

	return { events, connected };
}
