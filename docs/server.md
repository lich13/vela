# Linux 服务器版与实例切换

Vela 可以在一台 Linux 服务器上以后台服务运行，管理那台机器上的 Codex、Claude Code、Grok Build 的网关、账号、配置和用量。本机的 Vela 通过 SSH 连接它，在主窗口和快捷面板里切换“本机”与服务器。服务器版没有界面，也不开放任何新的网络端口。

## 服务器上有什么

- 程序 `/usr/local/bin/vela-server`，由 systemd 单元 `vela-server.service` 以 root 运行，数据目录是 `/root/.vela`。
- 网关仍只监听 `127.0.0.1:43721`，供同一台机器上的客户端使用。
- 控制通道是 Unix socket `/run/vela/rpc.sock`（仅 root 可读写）。本机 app 不直接连接它，而是经 SSH 在服务器上执行 `vela-server rpc-stdio`，由它在 SSH 通道和 socket 之间转发。
- 服务启动时恢复上次启用的网关；服务停止（`systemctl stop`、升级、关机）时先把各客户端的配置还原，再关闭监听。

## 安装与升级

从 GitHub Release 下载 `vela-server_<版本>_x86_64-linux.tar.gz`（x86_64，glibc 2.35 及以上，如 Ubuntu 22.04／24.04），用同一 Release 的 `SHA256SUMS` 校验后解压，以 root 执行：

```sh
sudo sh ./install.sh
```

脚本安装程序和 systemd 单元，设为开机启动并（重新）启动服务。升级重复同样的步骤；`/etc/vela/server.env` 和 `/root/.vela` 不会被改动。安装不会启用任何网关。

检查：

```sh
sudo vela-server --version
systemctl is-active vela-server
sudo vela-server health
```

`health` 在服务正常时输出 `ok`。

卸载：`sudo systemctl disable --now vela-server`，再删除 `/usr/local/bin/vela-server` 与 `/etc/systemd/system/vela-server.service`；数据在 `/root/.vela`，需要时自行删除。

## 客户端不在 root 下运行时

服务以 root 运行，默认管理 root 家目录下的客户端配置（`/root/.codex`、`/root/.grok` 等）。某个客户端由另一个用户运行时，在 `/etc/vela/server.env` 里指明它的目录后重启服务。例如 Claude Code 由 `ubuntu` 运行：

```
CLAUDE_CONFIG_DIR=/home/ubuntu/.claude
```

- Vela 以 root 写进别人目录的文件和新建的子目录，属主沿用所在目录的属主，那个用户仍能读写。
- Claude 的全局文件 `.claude.json`：目录名为 `.claude` 且目录内没有这个文件、而上一层有时，使用上一层那份（即该用户不设 `CLAUDE_CONFIG_DIR` 时 Claude Code 自己使用的位置）；否则使用目录内的。
- 每个客户端只管理一个目录。同一台机器上其他用户下的多余副本不会被读取或修改。

## 在本机添加服务器

1. 先让 `ssh <别名>` 在终端里能免密码登录：在 `~/.ssh/config` 里为服务器配置 `Host` 别名和密钥，并至少手动连接一次以记录主机指纹。Vela 不保存密码，也不会替你确认新的主机指纹。
2. 主窗口侧栏顶部的实例选择器 →“管理实例”→“添加服务器”，填写名称、SSH 主机别名，可选备用别名（主别名连不上时改用，例如经 Tailscale 的另一条路径）。登录用户不是 root 时勾选使用 sudo，要求该用户可以免密码执行 `sudo`。
3. “测试连接”成功会显示服务器版本。保存后在实例选择器里选择它。

别名只能包含字母、数字、点、下划线和连字符，不能以连字符开头；主机、端口、用户和密钥都写在 `~/.ssh/config` 里，Vela 只传别名。SSH 以非交互方式运行（不询问密码、不接受未知主机指纹、不使用连接复用和端口转发），远端命令固定为 `vela-server rpc-stdio`。

## 切换实例

- 主窗口侧栏顶部和快捷面板工具栏都有实例选择（快捷面板在添加过服务器后才显示），切的是同一个选择；选中的实例跨重启保留。
- 切到服务器前先握手，握手失败时保持原实例并显示错误。连接中断时实例显示“离线”，下一次操作或点“重试”时重新连接；握手最多等待 15 秒，每次操作最多等待 60 秒。超时的操作可能仍在服务器上执行，Vela 不会自动重发，请刷新后确认。
- 选中服务器时，主窗口的网关、账号、配置、用量、异常日志、供应商检测等页面显示并操作服务器上的数据；本机的网关照常运行，菜单栏／托盘图标仍反映本机。
- 主题、通知等界面偏好始终用本机的。
- 本机与服务器版本不同也可以连接：服务器不认识的操作会提示不支持。

## 服务器实例不支持的功能

- Antigravity（`agy`）网关接管，以及 Antigravity 账号的切换、导入和导出。`agy` 的额度仍可读取（经服务器上的 `agy` 命令）。
- 需要浏览器的登录：Codex／ChatGPT 账号登录、Claude 官方登录。请在服务器上用客户端自己的命令登录；已有的 Codex 账号可以切换。
- 需要本机文件对话框的导入导出（账号、账单、价格表等）。
- 查看已保存供应商的完整令牌；编辑供应商时令牌留空表示不修改。
- 强制退出客户端进程、合盖不休眠、系统通知、开机启动和 `ccswitch:` 链接。

这些入口在选中服务器时隐藏或停用。
