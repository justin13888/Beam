import { Link, useNavigate } from "@tanstack/react-router";
import {
	ClipboardType,
	Home,
	Library,
	LogIn,
	LogOut,
	Menu,
	Network,
	User,
	X,
} from "lucide-react";
import { useState } from "react";
import { useAuth } from "../hooks/auth";
import { Button } from "./ui/button";

export default function Header() {
	const [isOpen, setIsOpen] = useState(false);
	const { isAuthenticated, user, logout } = useAuth();
	const navigate = useNavigate();

	const handleLogout = () => {
		logout();
		setIsOpen(false);
		navigate({ to: "/login" });
	};

	return (
		<>
			<header className="p-4 flex items-center justify-between bg-gray-800 text-white shadow-lg">
				<div className="flex items-center">
					<button
						type="button"
						onClick={() => setIsOpen(true)}
						className="p-2 hover:bg-gray-700 rounded-lg transition-colors mr-4"
						aria-label="Open menu"
					>
						<Menu size={24} />
					</button>
					<h1 className="text-xl font-semibold">
						<Link to="/">
							<span className="text-cyan-400 font-bold">Beam</span>
						</Link>
					</h1>
				</div>

				<div className="hidden md:flex items-center gap-4">
					{isAuthenticated ? (
						<div className="flex items-center gap-4">
							<Link
								to="/profile"
								className="flex items-center gap-2 text-sm text-gray-300 hover:text-white transition-colors"
							>
								<User size={16} />
								<span>Hello, {user?.username}</span>
							</Link>
							<Button
								variant="ghost"
								size="sm"
								onClick={() => logout()}
								className="text-gray-300 hover:text-white hover:bg-gray-700"
							>
								<LogOut size={18} className="mr-2" />
								Logout
							</Button>
						</div>
					) : (
						<Link to="/login">
							<Button
								variant="outline"
								size="sm"
								className="bg-transparent text-white border-gray-600 hover:bg-gray-700"
							>
								<LogIn size={18} className="mr-2" />
								Sign In
							</Button>
						</Link>
					)}
				</div>
			</header>

			<aside
				className={`fixed top-0 left-0 h-full w-80 bg-gray-900 text-white shadow-2xl z-50 transform transition-transform duration-300 ease-in-out flex flex-col ${
					isOpen ? "translate-x-0" : "-translate-x-full"
				}`}
			>
				<div className="flex items-center justify-between p-4 border-b border-gray-700">
					<h2 className="text-xl font-bold">Navigation</h2>
					<button
						type="button"
						onClick={() => setIsOpen(false)}
						className="p-2 hover:bg-gray-800 rounded-lg transition-colors"
						aria-label="Close menu"
					>
						<X size={24} />
					</button>
				</div>

				<nav className="flex-1 p-4 overflow-y-auto">
					<Link
						to="/"
						onClick={() => setIsOpen(false)}
						className="flex items-center gap-3 p-3 rounded-lg hover:bg-gray-800 transition-colors mb-2"
						activeProps={{
							className:
								"flex items-center gap-3 p-3 rounded-lg bg-cyan-600 hover:bg-cyan-700 transition-colors mb-2",
						}}
					>
						<Home size={20} />
						<span className="font-medium">Home</span>
					</Link>

					{/* Demo Links Start */}

					<Link
						to="/libraries"
						onClick={() => setIsOpen(false)}
						className="flex items-center gap-3 p-3 rounded-lg hover:bg-gray-800 transition-colors mb-2"
						activeProps={{
							className:
								"flex items-center gap-3 p-3 rounded-lg bg-cyan-600 hover:bg-cyan-700 transition-colors mb-2",
						}}
					>
						<Library size={20} />
						<span className="font-medium">Libraries</span>
					</Link>

					<Link
						to="/demo/form/simple"
						onClick={() => setIsOpen(false)}
						className="flex items-center gap-3 p-3 rounded-lg hover:bg-gray-800 transition-colors mb-2"
						activeProps={{
							className:
								"flex items-center gap-3 p-3 rounded-lg bg-cyan-600 hover:bg-cyan-700 transition-colors mb-2",
						}}
					>
						<ClipboardType size={20} />
						<span className="font-medium">Simple Form</span>
					</Link>

					<Link
						to="/demo/form/address"
						onClick={() => setIsOpen(false)}
						className="flex items-center gap-3 p-3 rounded-lg hover:bg-gray-800 transition-colors mb-2"
						activeProps={{
							className:
								"flex items-center gap-3 p-3 rounded-lg bg-cyan-600 hover:bg-cyan-700 transition-colors mb-2",
						}}
					>
						<ClipboardType size={20} />
						<span className="font-medium">Address Form</span>
					</Link>

					<Link
						to="/demo/tanstack-query"
						onClick={() => setIsOpen(false)}
						className="flex items-center gap-3 p-3 rounded-lg hover:bg-gray-800 transition-colors mb-2"
						activeProps={{
							className:
								"flex items-center gap-3 p-3 rounded-lg bg-cyan-600 hover:bg-cyan-700 transition-colors mb-2",
						}}
					>
						<Network size={20} />
						<span className="font-medium">TanStack Query</span>
					</Link>

					{/* Demo Links End */}
				</nav>

				<div className="p-4 border-t border-gray-700">
					{isAuthenticated ? (
						<div className="space-y-4">
							<Link
								to="/profile"
								className="block"
								onClick={() => setIsOpen(false)}
							>
								<div className="flex items-center gap-3 px-3 py-2 text-gray-300 hover:bg-gray-800 rounded-lg transition-colors">
									<User size={20} />
									<span className="font-medium truncate">{user?.username}</span>
								</div>
							</Link>
							<button
								type="button"
								onClick={handleLogout}
								className="w-full flex items-center gap-3 p-3 rounded-lg hover:bg-red-900/50 text-red-400 transition-colors"
							>
								<LogOut size={20} />
								<span className="font-medium">Logout</span>
							</button>
						</div>
					) : (
						<div className="space-y-2">
							<Link
								to="/login"
								onClick={() => setIsOpen(false)}
								className="block w-full"
							>
								<Button className="w-full bg-cyan-600 hover:bg-cyan-700 text-white">
									Sign In
								</Button>
							</Link>
							<Link
								to="/register"
								onClick={() => setIsOpen(false)}
								className="block w-full"
							>
								<Button
									variant="outline"
									className="w-full border-gray-600 text-gray-300 hover:text-white hover:bg-gray-700"
								>
									Create Account
								</Button>
							</Link>
						</div>
					)}
				</div>
			</aside>
		</>
	);
}
