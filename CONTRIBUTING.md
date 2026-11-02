# 贡献与开发

先读 AGENTS.md、README.md、docs/HANDOFF.zh-CN.md 与 ADR-001。
需求/设计变更同步更新文档；新增真实能力必须有对应测试，不将模型输入当实际网络证据。

建议本地命令：

```sh
python scripts/check_repository.py
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

当前包没有自动部署和自动发布。提交合成测试数据，避免真实用户文件、凭据、绝对路径和未经脱敏的抓包。
许可证尚未由所有者选择，不自行假定仓库已经采用某开源许可证。
