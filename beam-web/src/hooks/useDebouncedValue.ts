import { useEffect, useState } from "react";

/** Returns `value`, delayed by `delayMs` after it last changed -- used to
 * turn keystroke-driven state into a settled value worth firing a network
 * request for (instant search without a request per keystroke). */
export function useDebouncedValue<T>(value: T, delayMs: number): T {
	const [debounced, setDebounced] = useState(value);

	useEffect(() => {
		const timer = setTimeout(() => setDebounced(value), delayMs);
		return () => clearTimeout(timer);
	}, [value, delayMs]);

	return debounced;
}
