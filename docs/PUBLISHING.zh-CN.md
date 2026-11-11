# 上传与验证

仓库已完成初始化，原始 38 个文件已上传并核验；详见 [上传验收记录](../reports/archive/GITHUB_PUBLISH_VERIFICATION.zh-CN.md)。此前 403 属于已解决的历史权限问题。
当前仓库已非空，不要再次运行仅供空仓库使用的 `scripts/publish.ps1`。

## 权限

需要具有写入能力的连接及有权写入 `wmqfl861/AK-sending` 的账户。集成的仓库授权与账户角色均须正确。
Contents 写入权限用于源码；本包没有增加 GitHub Actions workflow，因此不额外要求 workflows 写入。
标准只读连接仅勾选仓库不会变成可写。不要把 Token、密码或 SSH 私钥发到聊天或写入包内。
参考资料 S8、S9 见 SOURCES.zh-CN.md。

## Windows 空仓库初始化

解压后在项目根目录运行 `powershell -File scripts/publish.ps1`，先阅读脚本。需要本机 Git 已安装并已安全登录、有有效的 Git 作者配置。
脚本仅接受目标远端为空、当前目录没有已有 .git 的场景，执行仓库检查后新建本地 main 提交，再进行普通 push。
不使用 --force、不改全局 Git 设置、不自动注册账户、不写入明文凭据。若远端或本地已存在历史则停止，需按已有历史人工整合。
本次没有执行此脚本；不能把脚本存在理解为上传成功。

## 已有仓库

先读取远端最新提交及 AGENTS，将本包作为新分支变更整合，保留并审查已有内容；不直接覆盖。
合并和部署分别授权。普通 git push 失败不得改用 force。

## 成功证据

必须记录最终远端分支、完整提交 SHA、tree SHA 和关键文件回读。核对档案中所有源码与文档确实进入提交；排除 target、密钥、真实文件、缓存和其他项目资料。
本地测试结果与 GitHub CI 结果分开；本包未配置或触发远端 CI。
