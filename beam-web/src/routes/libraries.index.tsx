import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute, Link } from "@tanstack/react-router";
import {
	ChevronRight,
	Clock,
	FileVideo,
	FolderOpen,
	Library as LibraryIcon,
	Plus,
	RefreshCw,
	Scan,
	Trash2,
} from "lucide-react";
import { useId, useState } from "react";
import { toast } from "sonner";
import type { components } from "@/api.gen";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { useAuth } from "@/hooks/auth";
import { apiClient } from "@/lib/apiClient";
import { apiError } from "@/lib/problem";
import { RouteError } from "../components/RouteError";

type Library = components["schemas"]["beam_server.models.library.Library"];

// `/libraries/` -- an index route, not a layout.
//
// While this file was `libraries.tsx` the router treated it as the *parent* of
// `/libraries/$id`, and it renders no `<Outlet />`, so navigating to a library
// detail page silently rendered the library list instead. Every test mocked the
// router away, so nothing could see it.
export const Route = createFileRoute("/libraries/")({
	errorComponent: RouteError,
	component: LibrariesPage,
});

function formatTimeAgo(dateStr: unknown): string {
	if (!dateStr) return "Never";
	const date = new Date(dateStr as string);
	const now = new Date();
	const diffMs = now.getTime() - date.getTime();
	const diffSecs = Math.floor(diffMs / 1000);
	const diffMins = Math.floor(diffSecs / 60);
	const diffHrs = Math.floor(diffMins / 60);
	const diffDays = Math.floor(diffHrs / 24);

	if (diffSecs < 60) return "Just now";
	if (diffMins < 60) return `${diffMins}m ago`;
	if (diffHrs < 24) return `${diffHrs}h ago`;
	if (diffDays < 30) return `${diffDays}d ago`;
	return date.toLocaleDateString();
}

function ScanStatusBadge({ library }: { library: Library }) {
	const isScanning =
		library.last_scan_started_at &&
		(!library.last_scan_finished_at ||
			new Date(library.last_scan_started_at) >
				new Date(library.last_scan_finished_at));

	if (isScanning) {
		return (
			<span className="inline-flex items-center gap-1.5 px-2.5 py-1 rounded-full text-xs font-medium bg-amber-500/15 text-amber-400 border border-amber-500/20">
				<RefreshCw size={12} className="animate-spin" />
				Scanning...
			</span>
		);
	}

	if (library.last_scan_finished_at) {
		return (
			<span className="inline-flex items-center gap-1.5 px-2.5 py-1 rounded-full text-xs font-medium bg-emerald-500/15 text-emerald-400 border border-emerald-500/20">
				<Clock size={12} />
				{formatTimeAgo(library.last_scan_finished_at)}
			</span>
		);
	}

	return (
		<span className="inline-flex items-center gap-1.5 px-2.5 py-1 rounded-full text-xs font-medium bg-gray-500/15 text-gray-400 border border-gray-500/20">
			Not scanned
		</span>
	);
}

export function LibrariesPage() {
	const { isAuthenticated } = useAuth();
	const queryClient = useQueryClient();
	const {
		data,
		isLoading: loading,
		error,
		refetch,
	} = useQuery({
		queryKey: ["libraries"],
		queryFn: async () => {
			const { data, error } = await apiClient.GET("/v1/libraries", {
				credentials: "include",
			});
			if (error) throw apiError(error, "Failed to load libraries");
			return data;
		},
		enabled: isAuthenticated,
	});

	const invalidateLibraries = () =>
		queryClient.invalidateQueries({ queryKey: ["libraries"] });

	const createLibraryMutation = useMutation({
		mutationFn: async (vars: { name: string; rootPath: string }) => {
			const { data, error } = await apiClient.POST("/v1/admin/libraries", {
				body: { name: vars.name, root_path: vars.rootPath },
				credentials: "include",
			});
			if (error) throw apiError(error, "Failed to create library");
			return data;
		},
	});

	const scanLibraryMutation = useMutation({
		mutationFn: async (libraryId: string) => {
			const { error } = await apiClient.POST("/v1/admin/libraries/{id}/scan", {
				params: { path: { id: libraryId } },
				credentials: "include",
			});
			if (error) throw apiError(error, "Failed to scan library");
		},
	});

	const deleteLibraryMutation = useMutation({
		mutationFn: async (libraryId: string) => {
			const { error, response } = await apiClient.DELETE(
				"/v1/admin/libraries/{id}",
				{
					params: { path: { id: libraryId } },
					credentials: "include",
				},
			);
			if (error || !response.ok)
				throw apiError(error, "Failed to delete library");
		},
	});

	const [name, setName] = useState("");
	const [rootPath, setRootPath] = useState("");
	const [showCreateForm, setShowCreateForm] = useState(false);
	const libNameId = useId();
	const libRootPathId = useId();
	const [scanningIds, setScanningIds] = useState<Set<string>>(new Set());

	const handleCreate = async (e: React.FormEvent) => {
		e.preventDefault();
		try {
			await createLibraryMutation.mutateAsync({ name, rootPath });
			invalidateLibraries();
			setName("");
			setRootPath("");
			setShowCreateForm(false);
			toast.success(`Library "${name}" created`);
		} catch {
			// createLibraryMutation.error surfaces the error in the form
		}
	};

	const handleScan = async (libraryId: string, libraryName: string) => {
		setScanningIds((prev) => new Set(prev).add(libraryId));
		try {
			await scanLibraryMutation.mutateAsync(libraryId);
			invalidateLibraries();
			toast.info(`Scan started for "${libraryName}"`);
		} catch (err) {
			toast.error(
				`Failed to scan "${libraryName}": ${err instanceof Error ? err.message : "unknown error"}`,
			);
		} finally {
			setScanningIds((prev) => {
				const next = new Set(prev);
				next.delete(libraryId);
				return next;
			});
		}
	};

	const handleDelete = async (libraryId: string, libraryName: string) => {
		if (
			!confirm(
				`Are you sure you want to delete "${libraryName}"? This will remove all indexed files.`,
			)
		) {
			return;
		}
		try {
			await deleteLibraryMutation.mutateAsync(libraryId);
			invalidateLibraries();
			toast.success(`Library "${libraryName}" deleted`);
		} catch (err) {
			toast.error(
				`Failed to delete "${libraryName}": ${err instanceof Error ? err.message : "unknown error"}`,
			);
		}
	};

	const libraries = data ?? [];
	const creating = createLibraryMutation.isPending;
	const createError = createLibraryMutation.error;

	return (
		<div className="min-h-screen bg-gradient-to-br from-gray-950 via-gray-900 to-gray-950">
			<div className="max-w-7xl mx-auto px-6 py-8">
				{/* Header */}
				<div className="flex items-center justify-between mb-8">
					<div>
						<h1 className="text-3xl font-bold text-white flex items-center gap-3">
							<LibraryIcon className="text-cyan-400" size={32} />
							Libraries
						</h1>
						<p className="text-gray-400 mt-1">
							Manage your media libraries and file indexing
						</p>
					</div>
					<div className="flex gap-3">
						<Button
							onClick={() => refetch()}
							variant="outline"
							className="border-gray-700 text-gray-300 hover:bg-gray-800 hover:text-white"
						>
							<RefreshCw size={16} className="mr-2" />
							Refresh
						</Button>
						<Button
							onClick={() => setShowCreateForm(!showCreateForm)}
							className="bg-cyan-600 hover:bg-cyan-700 text-white"
						>
							<Plus size={16} className="mr-2" />
							Add Library
						</Button>
					</div>
				</div>

				{/* Create Form */}
				{showCreateForm && (
					<div className="mb-8 p-6 rounded-xl bg-gray-800/60 backdrop-blur-sm border border-gray-700/50 shadow-lg">
						<h2 className="text-xl font-semibold text-white mb-4">
							Create New Library
						</h2>
						<form
							onSubmit={handleCreate}
							className="flex flex-col sm:flex-row gap-4 items-end"
						>
							<div className="flex-1 space-y-2">
								<Label htmlFor={libNameId} className="text-gray-300">
									Name
								</Label>
								<Input
									id={libNameId}
									value={name}
									onChange={(e) => setName(e.target.value)}
									placeholder="e.g. Movies"
									required
									className="bg-gray-900/60 border-gray-600 text-white placeholder-gray-500 focus:border-cyan-500"
								/>
							</div>
							<div className="flex-1 space-y-2">
								<Label htmlFor={libRootPathId} className="text-gray-300">
									Root Path
								</Label>
								<Input
									id={libRootPathId}
									value={rootPath}
									onChange={(e) => setRootPath(e.target.value)}
									placeholder="/media/movies"
									required
									className="bg-gray-900/60 border-gray-600 text-white placeholder-gray-500 focus:border-cyan-500"
								/>
							</div>
							<div className="flex gap-2">
								<Button
									type="button"
									variant="outline"
									onClick={() => setShowCreateForm(false)}
									className="border-gray-600 text-gray-300 hover:bg-gray-700"
								>
									Cancel
								</Button>
								<Button
									type="submit"
									disabled={creating}
									className="bg-cyan-600 hover:bg-cyan-700 text-white"
								>
									{creating ? (
										<RefreshCw size={16} className="animate-spin mr-2" />
									) : (
										<Plus size={16} className="mr-2" />
									)}
									Create
								</Button>
							</div>
						</form>
						{createError && (
							<p className="text-red-400 text-sm mt-3">
								Error: {createError.message}
							</p>
						)}
					</div>
				)}

				{/* Libraries Grid */}
				{loading ? (
					<div className="flex items-center gap-3 text-gray-400 py-12">
						<RefreshCw className="animate-spin" size={20} />
						<span className="text-lg">Loading libraries...</span>
					</div>
				) : error ? (
					<div className="text-center py-12 space-y-4">
						<p className="text-red-400 text-lg">Error: {error.message}</p>
						<Button
							onClick={() => refetch()}
							variant="outline"
							className="border-gray-600 text-gray-300 hover:bg-gray-800"
						>
							Retry
						</Button>
					</div>
				) : libraries.length === 0 ? (
					<div className="text-center py-20 rounded-xl border border-dashed border-gray-700 bg-gray-800/20">
						<FolderOpen className="mx-auto text-gray-600 mb-4" size={56} />
						<h3 className="text-xl font-semibold text-gray-400 mb-2">
							No libraries yet
						</h3>
						<p className="text-gray-500 mb-6">
							Create your first library to start indexing media files.
						</p>
						<Button
							onClick={() => setShowCreateForm(true)}
							className="bg-cyan-600 hover:bg-cyan-700 text-white"
						>
							<Plus size={16} className="mr-2" />
							Create Library
						</Button>
					</div>
				) : (
					<div className="grid grid-cols-1 md:grid-cols-2 xl:grid-cols-3 gap-5">
						{libraries.map((lib) => (
							<div
								key={lib.id}
								className="group relative rounded-xl bg-gray-800/40 backdrop-blur-sm border border-gray-700/50 hover:border-cyan-500/30 transition-all duration-300 hover:shadow-lg hover:shadow-cyan-500/5 overflow-hidden"
							>
								{/* Card Header */}
								<div className="p-5 pb-3">
									<div className="flex items-start justify-between mb-3">
										<div className="flex-1 min-w-0">
											<h3 className="font-semibold text-lg text-white truncate">
												{lib.name}
											</h3>
											{lib.description && (
												<p className="text-sm text-gray-400 mt-0.5 truncate">
													{lib.description}
												</p>
											)}
										</div>
										<ScanStatusBadge library={lib} />
									</div>

									{/* Stats */}
									<div className="flex items-center gap-4 text-sm text-gray-400 mb-4">
										<span className="flex items-center gap-1.5">
											<FileVideo size={14} className="text-gray-500" />
											{lib.size} files
										</span>
										{lib.last_scan_file_count != null && (
											<span className="flex items-center gap-1.5">
												<Scan size={14} className="text-gray-500" />
												{lib.last_scan_file_count} scanned
											</span>
										)}
									</div>
								</div>

								{/* Card Actions */}
								<div className="px-5 pb-4 flex items-center gap-2">
									<Link
										to="/libraries/$id"
										params={{ id: lib.id }}
										className="flex-1"
									>
										<Button
											variant="outline"
											className="w-full border-gray-600 text-gray-300 hover:bg-gray-700 hover:text-white text-sm"
										>
											View Files
											<ChevronRight size={14} className="ml-1" />
										</Button>
									</Link>
									<Button
										variant="outline"
										className="border-gray-600 text-gray-300 hover:bg-cyan-600/20 hover:text-cyan-400 hover:border-cyan-500/40"
										onClick={() => handleScan(lib.id, lib.name)}
										disabled={scanningIds.has(lib.id)}
										title="Scan library"
									>
										{scanningIds.has(lib.id) ? (
											<RefreshCw size={16} className="animate-spin" />
										) : (
											<Scan size={16} />
										)}
									</Button>
									<Button
										variant="outline"
										className="border-gray-600 text-gray-300 hover:bg-red-600/20 hover:text-red-400 hover:border-red-500/40"
										onClick={() => handleDelete(lib.id, lib.name)}
										title="Delete library"
									>
										<Trash2 size={16} />
									</Button>
								</div>
							</div>
						))}
					</div>
				)}
			</div>
		</div>
	);
}
