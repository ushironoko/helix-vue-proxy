# helix-vue-proxy

LSP proxy that enables full Vue language support in [Helix](https://helix-editor.com/) by bridging `vue-language-server` (v3) and `typescript-language-server`.

## Why

`vue-language-server` v3 uses a custom LSP protocol (`tsserver/request` / `tsserver/response`) to communicate with TypeScript. Neovim handles this via Lua, but Helix has no extension mechanism to forward these notifications. Without this proxy, all Vue completions, hover, diagnostics, and go-to-definition will time out.

## How it works

```
Helix <--(stdio)--> helix-vue-proxy <--(stdio)--> vue-language-server
                          |
                          +-- mirrors: initialize, didOpen/Change/Close/Save
                          +-- forwards: tsserver/request -> workspace/executeCommand
                          v
                    typescript-language-server
                    (@vue/typescript-plugin)
```

The proxy transparently passes all LSP messages between Helix and `vue-language-server`. When `vue-language-server` emits a `tsserver/request` notification, the proxy intercepts it and forwards it to an internal `typescript-language-server` instance as a `workspace/executeCommand` (`typescript.tsserverRequest`). The response is converted back to `tsserver/response` and sent to `vue-language-server`.

## Install

### Prerequisites

```bash
npm install -g vue-language-server typescript-language-server typescript @vue/typescript-plugin
```

### npm (recommended)

```bash
npm install -g helix-vue-proxy
```

Prebuilt binaries are available for:
- macOS (ARM64, x64)
- Linux (x64, glibc)

> **Note**: Some package managers (bun, pnpm) may not preserve the executable permission of the binary. If you get an `EACCES` error, run:
>
> ```bash
> chmod +x $(node -e "console.log(require.resolve('@helix-vue-proxy/cli-darwin-arm64/helix-vue-proxy'))")
> ```
>
> Replace `cli-darwin-arm64` with your platform (`cli-darwin-x64`, `cli-linux-x64-gnu`).

### Build from source

```bash
git clone https://github.com/ushironoko/helix-vue-proxy.git
cd helix-vue-proxy
cargo build --release
cp target/release/helix-vue-proxy ~/.local/bin/
```

## Helix configuration

Add to your `languages.toml`:

```toml
[[language]]
name = "vue"
auto-format = true
language-servers = ["vuels"]

[language-server.vuels]
command = "helix-vue-proxy"
args = [
  "--plugin-path", "/path/to/@vue/typescript-plugin",
  "--tsdk", "/path/to/typescript/lib",
]
```

Find the paths on your system:

```bash
# @vue/typescript-plugin
echo "$(npm root -g)/@vue/typescript-plugin"

# TypeScript SDK
echo "$(npm root -g)/typescript/lib"
```

## CLI options

| Option | Default | Description |
|---|---|---|
| `--plugin-path` | (required) | Path to `@vue/typescript-plugin` |
| `--tsdk` | (required) | Path to TypeScript SDK (`typescript/lib`) |
| `--vue-server-path` | `vue-language-server` | Path to `vue-language-server` binary |
| `--ts-server-path` | `typescript-language-server` | Path to `typescript-language-server` binary |
| `--log-level` | `warn` | Log level: `trace`, `debug`, `info`, `warn`, `error` |
| `--log-file` | (none) | Write logs to a file (in addition to stderr) |

## Debugging

Enable debug logging to see all message routing:

```toml
[language-server.vuels]
command = "helix-vue-proxy"
args = [
  "--plugin-path", "/path/to/@vue/typescript-plugin",
  "--tsdk", "/path/to/typescript/lib",
  "--log-level", "debug",
  "--log-file", "/tmp/helix-vue-proxy.log",
]
```

Then check the log:

```bash
tail -f /tmp/helix-vue-proxy.log
```

Or view stderr in Helix with `:log-open`.

## Known trade-offs

- **Dual ts-ls instances**: Helix launches its own `typescript-language-server` for `.ts` files, and this proxy spawns another one internally. This doubles memory usage for TypeScript, but is unavoidable given Helix's architecture.

## License

MIT
