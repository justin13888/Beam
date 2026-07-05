import { useQuery } from "@tanstack/react-query";
import { createFileRoute, Link } from "@tanstack/react-router";
import {
	ChevronRight,
	FileVideo,
	Library,
	RefreshCw,
	Scan,
	Search,
} from "lucide-react";
import { ContinueWatchingRow } from "@/components/ContinueWatchingRow";
import { Button } from "@/components/ui/button";
import { useAuth } from "@/hooks/auth";
import { apiClient } from "@/lib/apiClient";

export const Route = createFileRoute("/")({
	component: DashboardPage,
});

function DashboardPage() {
	const { isAuthenticated } = useAuth();
	const { data, isLoading: loading } = useQuery({
		queryKey: ["libraries"],
		queryFn: async () => {
			const { data, error } = await apiClient.GET("/v1/libraries", {
				credentials: "include",
			});
			if (error) throw new Error("Failed to load libraries");
			return data;
		},
		enabled: isAuthenticated,
	});
	const libraries = data ?? [];
	const totalFiles = libraries.reduce((sum, lib) => sum + lib.size, 0);

	return (
		<div className="min-h-screen bg-gradient-to-br from-gray-950 via-gray-900 to-gray-950">
			<div className="max-w-5xl mx-auto px-6 py-16">
				{/* Hero */}
				<div className="text-center mb-16">
					<h1 className="text-5xl font-bold text-white mb-4">
						Welcome to{" "}
						<span className="bg-gradient-to-r from-cyan-400 to-blue-500 bg-clip-text text-transparent">
							Beam
						</span>
					</h1>
					<p className="text-lg text-gray-400 max-w-xl mx-auto">
						Your personal media server. Organize, index, and stream your media
						collections.
					</p>
				</div>

				{/* Quick Stats */}
				{!loading && (
					<div className="grid grid-cols-1 sm:grid-cols-3 gap-4 mb-12">
						<div className="rounded-xl bg-gray-800/40 backdrop-blur-sm border border-gray-700/50 p-5 text-center">
							<Library className="mx-auto text-cyan-400 mb-2" size={28} />
							<div className="text-3xl font-bold text-white">
								{libraries.length}
							</div>
							<div className="text-sm text-gray-400 mt-1">Libraries</div>
						</div>
						<div className="rounded-xl bg-gray-800/40 backdrop-blur-sm border border-gray-700/50 p-5 text-center">
							<FileVideo className="mx-auto text-purple-400 mb-2" size={28} />
							<div className="text-3xl font-bold text-white">{totalFiles}</div>
							<div className="text-sm text-gray-400 mt-1">Total Files</div>
						</div>
						<div className="rounded-xl bg-gray-800/40 backdrop-blur-sm border border-gray-700/50 p-5 text-center">
							<Scan className="mx-auto text-emerald-400 mb-2" size={28} />
							<div className="text-3xl font-bold text-white">
								{libraries.filter((l) => l.last_scan_finished_at).length}
							</div>
							<div className="text-sm text-gray-400 mt-1">Scanned</div>
						</div>
					</div>
				)}

				{loading && (
					<div className="flex items-center justify-center py-12 text-gray-400">
						<RefreshCw className="animate-spin mr-2" size={18} />
						Loading...
					</div>
				)}

				{/* Quick Actions */}
				<div className="grid grid-cols-1 sm:grid-cols-2 gap-5">
					<Link to="/libraries" className="group">
						<div className="rounded-xl bg-gray-800/40 backdrop-blur-sm border border-gray-700/50 p-6 hover:border-cyan-500/30 transition-all duration-300 hover:shadow-lg hover:shadow-cyan-500/5">
							<div className="flex items-center justify-between">
								<div>
									<h3 className="text-lg font-semibold text-white flex items-center gap-2">
										<Library size={20} className="text-cyan-400" />
										Manage Libraries
									</h3>
									<p className="text-sm text-gray-400 mt-1">
										View, create, scan, and manage your media libraries
									</p>
								</div>
								<ChevronRight
									size={20}
									className="text-gray-600 group-hover:text-cyan-400 transition-colors"
								/>
							</div>
						</div>
					</Link>
					<Link to="/explore" className="group">
						<div className="rounded-xl bg-gray-800/40 backdrop-blur-sm border border-gray-700/50 p-6 hover:border-purple-500/30 transition-all duration-300 hover:shadow-lg hover:shadow-purple-500/5">
							<div className="flex items-center justify-between">
								<div>
									<h3 className="text-lg font-semibold text-white flex items-center gap-2">
										<Search size={20} className="text-purple-400" />
										Explore Media
									</h3>
									<p className="text-sm text-gray-400 mt-1">
										Search and browse your indexed media collection
									</p>
								</div>
								<ChevronRight
									size={20}
									className="text-gray-600 group-hover:text-purple-400 transition-colors"
								/>
							</div>
						</div>
					</Link>
				</div>

				<ContinueWatchingRow />

				{/* Recent Libraries */}
				{libraries.length > 0 && (
					<div className="mt-12">
						<div className="flex items-center justify-between mb-4">
							<h2 className="text-xl font-semibold text-white">
								Your Libraries
							</h2>
							<Link to="/libraries">
								<Button
									variant="ghost"
									className="text-gray-400 hover:text-white text-sm"
								>
									View all
									<ChevronRight size={14} className="ml-1" />
								</Button>
							</Link>
						</div>
						<div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 gap-4">
							{libraries.slice(0, 6).map((lib) => (
								<Link
									key={lib.id}
									to="/libraries/$id"
									params={{ id: lib.id }}
									className="group"
								>
									<div className="rounded-xl bg-gray-800/30 border border-gray-700/50 p-4 hover:border-cyan-500/20 transition-all duration-200">
										<h4 className="font-medium text-white truncate">
											{lib.name}
										</h4>
										<p className="text-xs text-gray-500 mt-1">
											{lib.size} files
											{lib.description && ` • ${lib.description}`}
										</p>
									</div>
								</Link>
							))}
						</div>
					</div>
				)}
			</div>
		</div>
	);
}
