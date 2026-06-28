# 内置离线包（多平台）

打包安装程序前，`build.rs` 会按当前构建 target 自动选择对应平台的内置包并断言其存在。
文件缺失时 release 模式会 panic，开发模式仅打警告。

## Node.js 内置包

| 平台 | 文件名 | 最小体积 | 用途 |
|------|--------|----------|------|
| Windows x64 | `node-v22.14.0-win-x64.zip` | 5 MB | NSIS 安装包内嵌 |
| macOS Apple Silicon | `node-v22.14.0-darwin-arm64.tar.gz` | 25 MB | `.app`/`.dmg` 内嵌（aarch64） |
| macOS Intel | `node-v22.14.0-darwin-x64.tar.gz` | 25 MB | `.app`/`.dmg` 内嵌（x86_64） |
| Linux x64 | `node-v22.14.0-linux-x64.tar.gz` | 25 MB | AppImage/Deb 内嵌 |

> 体积判定：macOS/Linux 的 tar.gz 包含完整运行时，约为 Windows zip 的 5 倍，
> 25 MB 为官方 tarball 实测最小值（包含 npm、npx 等子工具）。

## MinGit 内置包

| 平台 | 文件名 | 最小体积 | 用途 |
|------|--------|----------|------|
| Windows x64 | `MinGit-2.53.0-64-bit.zip` | 400 KB | NSIS 安装包内嵌 |
| macOS Apple Silicon | （不内置） | — | 依赖系统 git（Xcode CLT 自带） |
| macOS Intel | （不内置） | — | 依赖系统 git（Xcode CLT 自带） |
| Linux | （不内置，依赖系统包） | — | `apt install git` |

> macOS 始终通过 Xcode Command Line Tools 自带 git；`env_paths.rs` 在解析 git
> 路径时会优先探测 `/usr/bin/git` → `/opt/homebrew/bin/git` → `/usr/local/bin/git`，
> 因此无需在 Mac 上内置 MinGit。

## 获取方式

- **推荐**：在 Mac/Linux 主机上跑 `./src-tauri/scripts/download-bundles.sh`（自动按 `uname -m` 选择架构）。
- **离线场景**：手工将对应平台的 tar.gz/zip 放入本目录后执行 `cargo tauri build`。
- **CI**：可在环境变量中设置 `SKIP_BUNDLED_CHECK=true` 跳过断言，依赖安装期/首次运行时下载。

## 文件布局

下载完成后目录结构形如：

```
bundled-env/
├── README.md                              ← 本文件
├── node-v22.14.0-darwin-arm64.tar.gz      ← macOS Apple Silicon
├── node-v22.14.0-darwin-x64.tar.gz        ← macOS Intel
├── node-v22.14.0-linux-x64.tar.gz         ← Linux
├── node-v22.14.0-win-x64.zip             ← Windows
└── MinGit-2.53.0-64-bit.zip              ← Windows（macOS/Linux 不内置）
```

`build.rs` 的 `bundled_node_filename()` 与 `bundled_mingit_filename()` 会按
`#[cfg(target_os)]` + `#[cfg(target_arch)]` 自动挑选当前 target 需要的那个文件，
无需在源码中手动切换。
