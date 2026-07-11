import path from "path"
import { defineConfig, mergeConfig, type ConfigEnv, type UserConfig } from "vite"
import baseConfig from "./vite.config"

// Web build: identical to the desktop build except every @tauri-apps
// import is swapped for an HTTP/SSE shim (src-web/shims/*) that talks
// to the in-app RPC bridge. Upstream code is untouched.
//
//   dev:   npm run dev:web    (proxies bridge endpoints to :19829)
//   build: npm run build:web  (outputs to dist-web/)

const shim = (name: string) => path.resolve(__dirname, `./src-web/shims/${name}.ts`)

const BRIDGE_TARGET = process.env.LLM_WIKI_BRIDGE_URL ?? "http://127.0.0.1:19829"

export default defineConfig(async (env: ConfigEnv): Promise<UserConfig> => {
  const base = await (typeof baseConfig === "function" ? baseConfig(env) : baseConfig)
  return mergeConfig(base, {
    resolve: {
      alias: {
        "@tauri-apps/api/core": shim("core"),
        "@tauri-apps/api/event": shim("event"),
        "@tauri-apps/api/window": shim("window"),
        "@tauri-apps/plugin-http": shim("plugin-http"),
        "@tauri-apps/plugin-store": shim("plugin-store"),
        "@tauri-apps/plugin-dialog": shim("plugin-dialog"),
        "@tauri-apps/plugin-opener": shim("plugin-opener"),
        "@tauri-apps/plugin-autostart": shim("plugin-autostart"),
      },
    },
    build: {
      outDir: "dist-web",
    },
    server: {
      proxy: {
        "/rpc": BRIDGE_TARGET,
        "/store": BRIDGE_TARGET,
        "/asset": BRIDGE_TARGET,
        "/events": BRIDGE_TARGET,
        "/proxy": BRIDGE_TARGET,
        "/upload": BRIDGE_TARGET,
      },
    },
  } satisfies UserConfig)
})
