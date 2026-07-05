import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute, Link } from "@tanstack/react-router";
import {
	AlertTriangle,
	ArrowLeft,
	CheckCircle2,
	ChevronDown,
	ChevronUp,
	CircleDot,
	Clock,
	FileQuestion,
	FileVideo,
	Film,
	Filter,
	HardDrive,
	RefreshCw,
	Scan,
	Search,
	Tv,
} from "lucide-react";
import { useMemo, useState } from "react";
import type { components } from "@/api.gen";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { useAuth } from "@/hooks/auth";
import { apiClient } from "@/lib/apiClient";
import { RouteError } from "../components/RouteError";

type LibraryFile =
	components["schemas"]["beam_server.models.library.file.LibraryFile"];
type FileIndexStatus =
	components["schemas"]["beam_server.models.library.file.FileIndexStatus"];
type FileContentType =
	components["schemas"]["beam_server.models.library.file.FileContentType"];

export const Route = createFileRoute("/libraries/$id")({
	errorComponent: RouteError,
	component: LibraryDetailPage,
});

function formatFileSize(bytes: number): string {
	if (bytes === 0) return "0 B";
	const k = 1024;
	const sizes = ["B", "KB", "MB", "GB", "TB"];
	const i = Math.floor(Math.log(bytes) / Math.log(k));
	return `${Number.parseFloat((bytes / k ** i).toFixed(1))} ${sizes[i]}`;
}

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

type SortField = "path" | "sizeBytes" | "status" | "contentType" | "updatedAt";
type SortDirection = "asc" | "desc";

function StatusIcon({ status }: { status: FileIndexStatus }) {
	switch (status) {
		case "Known":
			return <CheckCircle2 size={16} className="text-emerald-400" />;
		case "Changed":
			return <AlertTriangle size={16} className="text-amber-400" />;
		case "Unknown":
			return <CircleDot size={16} className="text-gray-400" />;
	}
}

function StatusBadge({ status }: { status: FileIndexStatus }) {
	const styles: Record<FileIndexStatus, string> = {
		Known: "bg-emerald-500/15 text-emerald-400 border-emerald-500/20",
		Changed: "bg-amber-500/15 text-amber-400 border-amber-500/20",
		Unknown: "bg-gray-500/15 text-gray-400 border-gray-500/20",
	};

	const labels: Record<FileIndexStatus, string> = {
		Known: "Indexed",
		Changed: "Changed",
		Unknown: "Unknown",
	};

	return (
		<span
			className={`inline-flex items-center gap-1.5 px-2 py-0.5 rounded-full text-xs font-medium border ${styles[status]}`}
		>
			<StatusIcon status={status} />
			{labels[status]}
		</span>
	);
}

function ContentTypeBadge({ contentType }: { contentType: FileContentType }) {
	switch (contentType) {
		case "Movie":
			return (
				<span className="inline-flex items-center gap-1 text-xs text-purple-400">
					<Film size={12} />
					Movie
				</span>
			);
		case "Episode":
			return (
				<span className="inline-flex items-center gap-1 text-xs text-blue-400">
					<Tv size={12} />
					Episode
				</span>
			);
		case "Unclassified":
			return (
				<span className="inline-flex items-center gap-1 text-xs text-gray-500">
					<FileQuestion size={12} />
					Unclassified
				</span>
			);
	}
}

function SortHeader({
	label,
	field,
	currentField,
	currentDirection,
	onSort,
}: {
	label: string;
	field: SortField;
	currentField: SortField;
	currentDirection: SortDirection;
	onSort: (field: SortField) => void;
}) {
	const active = currentField === field;
	return (
		<button
			type="button"
			onClick={() => onSort(field)}
			className={`flex items-center gap-1 text-xs font-medium uppercase tracking-wider hover:text-white transition-colors ${active ? "text-cyan-400" : "text-gray-500"}`}
		>
			{label}
			{active &&
				(currentDirection === "asc" ? (
					<ChevronUp size={12} />
				) : (
					<ChevronDown size={12} />
				))}
		</button>
	);
}

function LibraryDetailPage() {
	const { id } = Route.useParams();
	const { token } = useAuth();
	const queryClient = useQueryClient();

	const {
		data: library,
		isLoading: libraryLoading,
		error: libraryError,
		refetch: refetchLibrary,
	} = useQuery({
		queryKey: ["library", id],
		queryFn: async () => {
			const { data, error } = await apiClient.GET("/v1/libraries/{id}", {
				params: {
					path: { id },
					header: { Authorization: `Bearer ${token}` },
				},
			});
			if (error) throw new Error("Failed to load library");
			return data;
		},
		enabled: !!token,
	});

	const {
		data: filesData,
		isLoading: filesLoading,
		error: filesError,
		refetch: refetchFiles,
	} = useQuery({
		queryKey: ["library", id, "files"],
		queryFn: async () => {
			const { data, error } = await apiClient.GET("/v1/libraries/{id}/files", {
				params: {
					path: { id },
					header: { Authorization: `Bearer ${token}` },
				},
			});
			if (error) throw new Error("Failed to load library files");
			return data;
		},
		enabled: !!token,
	});

	const loading = libraryLoading || filesLoading;
	const error = libraryError ?? filesError;
	const refetch = () => {
		refetchLibrary();
		refetchFiles();
	};

	const scanLibraryMutation = useMutation({
		mutationFn: async () => {
			const { error } = await apiClient.POST("/v1/admin/libraries/{id}/scan", {
				params: {
					path: { id },
					header: { Authorization: `Bearer ${token}` },
				},
			});
			if (error) throw new Error("Failed to scan library");
		},
		onSuccess: () => {
			queryClient.invalidateQueries({ queryKey: ["library", id] });
		},
	});

	const [scanning, setScanning] = useState(false);
	const [scanError, setScanError] = useState<string | null>(null);
	const [searchQuery, setSearchQuery] = useState("");
	const [statusFilter, setStatusFilter] = useState<FileIndexStatus | "all">(
		"all",
	);
	const [sortField, setSortField] = useState<SortField>("path");
	const [sortDirection, setSortDirection] = useState<SortDirection>("asc");

	const handleScan = async () => {
		setScanError(null);
		setScanning(true);
		try {
			await scanLibraryMutation.mutateAsync();
			refetch();
		} catch (err) {
			setScanError(err instanceof Error ? err.message : "Scan failed");
		} finally {
			setScanning(false);
		}
	};

	const handleSort = (field: SortField) => {
		if (sortField === field) {
			setSortDirection((d) => (d === "asc" ? "desc" : "asc"));
		} else {
			setSortField(field);
			setSortDirection("asc");
		}
	};

	const files = filesData ?? [];

	// Filter and sort files
	const filteredFiles = useMemo(() => {
		let result = [...files];

		// Search filter
		if (searchQuery) {
			const q = searchQuery.toLowerCase();
			result = result.filter(
				(f) =>
					f.path.toLowerCase().includes(q) ||
					f.mime_type?.toLowerCase().includes(q) ||
					f.container_format?.toLowerCase().includes(q),
			);
		}

		// Status filter
		if (statusFilter !== "all") {
			result = result.filter((f) => f.status === statusFilter);
		}

		// Sort
		result.sort((a, b) => {
			let cmp = 0;
			switch (sortField) {
				case "path":
					cmp = a.path.localeCompare(b.path);
					break;
				case "sizeBytes":
					cmp = a.size_bytes - b.size_bytes;
					break;
				case "status":
					cmp = a.status.localeCompare(b.status);
					break;
				case "contentType":
					cmp = a.content_type.localeCompare(b.content_type);
					break;
				case "updatedAt":
					cmp =
						new Date(a.updated_at).getTime() - new Date(b.updated_at).getTime();
					break;
			}
			return sortDirection === "asc" ? cmp : -cmp;
		});

		return result;
	}, [files, searchQuery, statusFilter, sortField, sortDirection]);

	// Stats
	const stats = useMemo(() => {
		const known = files.filter((f) => f.status === "Known").length;
		const changed = files.filter((f) => f.status === "Changed").length;
		const unknown = files.filter((f) => f.status === "Unknown").length;
		const totalSize = files.reduce((sum, f) => sum + f.size_bytes, 0);
		return { known, changed, unknown, totalSize };
	}, [files]);

	return (
		<div className="min-h-screen bg-linear-to-br from-gray-950 via-gray-900 to-gray-950">
			<div className="max-w-7xl mx-auto px-6 py-8">
				{/* Breadcrumb & Header — always visible */}
				<div className="mb-6">
					<Link
						to="/libraries"
						className="inline-flex items-center gap-1.5 text-sm text-gray-400 hover:text-cyan-400 transition-colors mb-4"
					>
						<ArrowLeft size={14} />
						Back to Libraries
					</Link>
					<div className="flex items-center justify-between">
						<div>
							{loading ? (
								<div className="h-9 w-48 bg-gray-700/40 rounded animate-pulse" />
							) : (
								<h1 className="text-3xl font-bold text-white">
									{library?.name ?? (error ? "Library" : "Library not found")}
								</h1>
							)}
							{!loading && !error && library?.description && (
								<p className="text-gray-400 mt-1">{library.description}</p>
							)}
						</div>
						<div className="flex flex-col items-end gap-2">
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
									onClick={handleScan}
									disabled={scanning || loading || !!error || !library}
									className="bg-cyan-600 hover:bg-cyan-700 text-white"
								>
									{scanning ? (
										<RefreshCw size={16} className="animate-spin mr-2" />
									) : (
										<Scan size={16} className="mr-2" />
									)}
									{scanning ? "Scanning..." : "Scan Library"}
								</Button>
							</div>
							{scanError && (
								<p className="text-red-400 text-sm">Scan failed: {scanError}</p>
							)}
						</div>
					</div>
				</div>

				{/* Body — conditional on loading/error/not-found */}
				{loading ? (
					<>
						{/* Skeleton stats cards */}
						<div className="grid grid-cols-2 md:grid-cols-4 gap-4 mb-8">
							{[...Array(4)].map((_, i) => (
								<div
									// biome-ignore lint/suspicious/noArrayIndexKey: skeleton placeholders have no identity
									key={i}
									className="rounded-xl bg-gray-800/40 backdrop-blur-sm border border-gray-700/50 p-4 animate-pulse"
								>
									<div className="h-4 bg-gray-700/50 rounded mb-3 w-24" />
									<div className="h-8 bg-gray-700/50 rounded w-12" />
								</div>
							))}
						</div>
						{/* Skeleton table */}
						<div className="rounded-xl border border-gray-700/50 overflow-hidden animate-pulse">
							<div className="h-10 bg-gray-800/60 border-b border-gray-700/50" />
							{[...Array(5)].map((_, i) => (
								<div
									// biome-ignore lint/suspicious/noArrayIndexKey: skeleton placeholders have no identity
									key={i}
									className="h-12 border-b border-gray-800/50 bg-gray-800/20"
								/>
							))}
						</div>
					</>
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
				) : !library ? (
					<div className="text-center py-12">
						<p className="text-gray-400 text-lg">Library not found</p>
					</div>
				) : (
					<>
						{/* Stats Cards */}
						<div className="grid grid-cols-2 md:grid-cols-4 gap-4 mb-8">
							<div className="rounded-xl bg-gray-800/40 backdrop-blur-sm border border-gray-700/50 p-4">
								<div className="flex items-center gap-2 text-gray-400 text-sm mb-1">
									<FileVideo size={14} />
									Total Files
								</div>
								<div className="text-2xl font-bold text-white">
									{files.length}
								</div>
							</div>
							<div className="rounded-xl bg-gray-800/40 backdrop-blur-sm border border-gray-700/50 p-4">
								<div className="flex items-center gap-2 text-emerald-400 text-sm mb-1">
									<CheckCircle2 size={14} />
									Indexed
								</div>
								<div className="text-2xl font-bold text-white">
									{stats.known}
								</div>
							</div>
							<div className="rounded-xl bg-gray-800/40 backdrop-blur-sm border border-gray-700/50 p-4">
								<div className="flex items-center gap-2 text-amber-400 text-sm mb-1">
									<AlertTriangle size={14} />
									Changed / Unknown
								</div>
								<div className="text-2xl font-bold text-white">
									{stats.changed + stats.unknown}
								</div>
							</div>
							<div className="rounded-xl bg-gray-800/40 backdrop-blur-sm border border-gray-700/50 p-4">
								<div className="flex items-center gap-2 text-gray-400 text-sm mb-1">
									<HardDrive size={14} />
									Total Size
								</div>
								<div className="text-2xl font-bold text-white">
									{formatFileSize(stats.totalSize)}
								</div>
							</div>
						</div>

						{/* Scan Info */}
						{library.last_scan_finished_at && (
							<div className="flex items-center gap-4 text-sm text-gray-400 mb-6 px-1">
								<span className="flex items-center gap-1.5">
									<Clock size={14} />
									Last scanned {formatTimeAgo(library.last_scan_finished_at)}
								</span>
								{library.last_scan_file_count != null && (
									<span>• {library.last_scan_file_count} files processed</span>
								)}
							</div>
						)}

						{/* Search & Filter Bar */}
						<div className="flex flex-col sm:flex-row gap-3 mb-6">
							<div className="relative flex-1">
								<Search
									size={16}
									className="absolute left-3 top-1/2 -translate-y-1/2 text-gray-500"
								/>
								<Input
									placeholder="Search files by path, type, or format..."
									value={searchQuery}
									onChange={(e) => setSearchQuery(e.target.value)}
									className="pl-10 bg-gray-800/60 border-gray-700 text-white placeholder-gray-500 focus:border-cyan-500"
								/>
							</div>
							<div className="flex items-center gap-2">
								<Filter size={16} className="text-gray-500" />
								<div className="flex rounded-lg border border-gray-700 overflow-hidden">
									{(
										[
											{ label: "All", value: "all" },
											{ label: "Indexed", value: "Known" },
											{ label: "Changed", value: "Changed" },
											{ label: "Unknown", value: "Unknown" },
										] as const
									).map((opt) => (
										<button
											key={opt.value}
											type="button"
											onClick={() => setStatusFilter(opt.value)}
											className={`px-3 py-1.5 text-xs font-medium transition-colors ${
												statusFilter === opt.value
													? "bg-cyan-600 text-white"
													: "bg-gray-800/60 text-gray-400 hover:bg-gray-700 hover:text-white"
											}`}
										>
											{opt.label}
										</button>
									))}
								</div>
							</div>
						</div>

						{/* File Table */}
						{filteredFiles.length === 0 ? (
							<div className="text-center py-16 rounded-xl border border-dashed border-gray-700 bg-gray-800/20">
								<FileQuestion
									className="mx-auto text-gray-600 mb-4"
									size={48}
								/>
								<h3 className="text-lg font-semibold text-gray-400 mb-2">
									{files.length === 0
										? "No files indexed"
										: "No files match your filters"}
								</h3>
								<p className="text-gray-500">
									{files.length === 0
										? "Run a scan to index files in this library."
										: "Try adjusting your search or filter criteria."}
								</p>
							</div>
						) : (
							<div className="rounded-xl border border-gray-700/50 overflow-hidden">
								{/* Table Header */}
								<div className="grid grid-cols-[1fr_100px_80px_100px_100px] gap-4 px-5 py-3 bg-gray-800/60 border-b border-gray-700/50">
									<SortHeader
										label="File Path"
										field="path"
										currentField={sortField}
										currentDirection={sortDirection}
										onSort={handleSort}
									/>
									<SortHeader
										label="Size"
										field="sizeBytes"
										currentField={sortField}
										currentDirection={sortDirection}
										onSort={handleSort}
									/>
									<SortHeader
										label="Status"
										field="status"
										currentField={sortField}
										currentDirection={sortDirection}
										onSort={handleSort}
									/>
									<SortHeader
										label="Content"
										field="contentType"
										currentField={sortField}
										currentDirection={sortDirection}
										onSort={handleSort}
									/>
									<SortHeader
										label="Updated"
										field="updatedAt"
										currentField={sortField}
										currentDirection={sortDirection}
										onSort={handleSort}
									/>
								</div>

								{/* Table Body */}
								<div className="divide-y divide-gray-800/50">
									{filteredFiles.map((file) => (
										<FileRow key={file.id} file={file} />
									))}
								</div>

								{/* Footer */}
								<div className="px-5 py-3 bg-gray-800/40 border-t border-gray-700/50 text-xs text-gray-500">
									Showing {filteredFiles.length} of {files.length} files
								</div>
							</div>
						)}
					</>
				)}
			</div>
		</div>
	);
}

function FileRow({ file }: { file: LibraryFile }) {
	// Extract just the filename from the full path
	const pathParts = file.path.split("/");
	const filename = pathParts[pathParts.length - 1];
	const directory = pathParts.slice(0, -1).join("/");

	return (
		<div className="grid grid-cols-[1fr_100px_80px_100px_100px] gap-4 px-5 py-3 hover:bg-gray-800/30 transition-colors group">
			{/* File Path */}
			<div className="min-w-0">
				<div className="flex items-center gap-2">
					<FileVideo size={14} className="text-gray-500 shrink-0" />
					<span className="text-sm text-white font-medium truncate">
						{filename}
					</span>
				</div>
				<p
					className="text-xs text-gray-500 truncate mt-0.5 ml-5"
					title={file.path}
				>
					{directory}
				</p>
			</div>

			{/* Size */}
			<div className="flex items-center">
				<span className="text-sm text-gray-300">
					{formatFileSize(file.size_bytes)}
				</span>
			</div>

			{/* Status */}
			<div className="flex items-center">
				<StatusBadge status={file.status} />
			</div>

			{/* Content Type */}
			<div className="flex items-center">
				<ContentTypeBadge contentType={file.content_type} />
			</div>

			{/* Updated */}
			<div className="flex items-center">
				<span className="text-xs text-gray-500">
					{formatTimeAgo(file.updated_at)}
				</span>
			</div>
		</div>
	);
}
