import type { CodegenConfig } from "@graphql-codegen/cli";

const config: CodegenConfig = {
	overwrite: true,
	// Use schema from beam-stream (source of truth)
	schema: "../beam-stream/schema.graphql",
	// This assumes that all your source files are in a top-level `src/` directory - you might need to adjust this to your file structure
	documents: ["src/**/*.{ts,tsx}"],
	// Don't exit with non-zero status when there are no documents
	ignoreNoDocuments: true,
	generates: {
		// Use a path that works the best for the structure of your application
		"./src/gql.ts": {
			plugins: ["typescript", "typescript-operations"],
			config: {
				avoidOptionals: {
					// Use `null` for nullable fields instead of optionals
					field: true,
					// Allow nullable input fields to remain unspecified
					inputValue: false,
				},
				// Use `unknown` instead of `any` for unconfigured scalars
				defaultScalarType: "unknown",
				// Map known scalars to concrete TypeScript types
				scalars: {
					DateTime: "string",
					Decimal: "string",
				},
				// Apollo Client always includes `__typename` fields
				nonOptionalTypename: true,
				// Apollo Client doesn't add the `__typename` field to root types so
				// don't generate a type for the `__typename` for root operation types.
				skipTypeNameForRoot: true,
			},
		},
	},
};

export default config;
