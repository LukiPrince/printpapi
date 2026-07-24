import type { NextConfig } from "next";

// In production the Python server serves this bundle AND the API from one origin, so every fetch
// in lib/api.ts is root-relative. `next dev` has no API behind it, so proxy those paths to a
// running printpapi. The key is only present in dev — `output: export` drops rewrites from the
// build and warns about them, and there is nothing to proxy there anyway.
const dev = process.env.NODE_ENV === "development";
const DEV_API = process.env.PRINTPAPI_ORIGIN ?? "http://127.0.0.1:3460";
const API_PATHS = ["/health", "/metrics", "/printers", "/jobs", "/apikeys"];

const nextConfig: NextConfig = {
  output: "export",
  trailingSlash: true, // /devices/ -> devices/index.html, so deep links work off a dumb file server
  images: { unoptimized: true },
  ...(dev && {
    rewrites: async () => ({
      // Both forms: trailingSlash 308s "/printers" to "/printers/" before rewrites are consulted.
      beforeFiles: API_PATHS.flatMap((p) => [
        { source: p, destination: DEV_API + p },
        { source: `${p}/`, destination: DEV_API + p },
        { source: `${p}/:rest*`, destination: `${DEV_API}${p}/:rest*` },
      ]),
    }),
  }),
};

export default nextConfig;
