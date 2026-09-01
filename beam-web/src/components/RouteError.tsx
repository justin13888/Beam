import { useRouter } from "@tanstack/react-router";
import { AlertTriangle, RefreshCw } from "lucide-react";
import { Button } from "@/components/ui/button";
import { ApiError } from "@/lib/problem";

export function RouteError({
	error,
	reset,
}: {
	error: Error;
	reset: () => void;
}) {
	const router = useRouter();

	const handleRetry = () => {
		reset();
		router.invalidate();
	};

	return (
		<div className="min-h-screen bg-gradient-to-br from-gray-950 via-gray-900 to-gray-950 flex items-center justify-center">
			<div className="text-center space-y-4">
				<AlertTriangle className="mx-auto text-amber-400" size={40} />
				<h2 className="text-xl font-semibold text-white">
					Something went wrong
				</h2>
				{/*
				 * An ApiError's message has already been through `apiError`,
				 * which shows the server's own `detail` only for a Beam
				 * problem document below 500 -- so it is a sentence written
				 * for a viewer, and showing it is the entire point of the
				 * error taxonomy. Anything else is an unvetted exception
				 * message and stays behind the DEV gate (NFR-108).
				 */}
				{error instanceof ApiError ? (
					<p className="text-gray-300 text-sm max-w-md mx-auto">
						{error.message}
					</p>
				) : (
					import.meta.env.DEV && (
						<p className="text-red-400 text-sm max-w-md mx-auto font-mono bg-gray-900/60 rounded p-3">
							{error.message}
						</p>
					)
				)}
				<Button
					onClick={handleRetry}
					variant="outline"
					className="border-gray-600 text-gray-300 hover:bg-gray-800"
				>
					<RefreshCw size={16} className="mr-2" />
					Try again
				</Button>
			</div>
		</div>
	);
}
