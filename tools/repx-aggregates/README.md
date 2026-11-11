# RepX Aggregate 互验

使用固定 PhysX 3.4.2 SDK 创建 PVD 捕获和参考 RepX，再由官方 XML 导入器读取 Rust 导出的 collection，通过 SDK getter 比较容量、自碰撞、成员、刚体和关节关系。

样例包含 Dynamic / Static 成员、空组、零容量组、跨组关节，以及成员移除、迁移和关节销毁。四个快照分别验证整帧导出和每个 Actor 的递归依赖闭包。

## 运行

先按 [SDK 生产器说明](../physx-sample/README.md) 准备依赖。构建脚本要求官方 SDK commit `5e42a5f112351a223c19c17bb331e6c55037b8eb`，且 SDK 受版本控制的文件未经修改。

在仓库根目录执行：

```powershell
.\tools\repx-aggregates\build.ps1
cargo build --locked -p pvd-cli
python tools/repx-aggregates/verify.py --output output/repx-aggregates-check
$env:PVD_REPX_AGGREGATE_DIRECTORY = (Resolve-Path 'output/repx-aggregates-check').Path
cargo test --locked -p pvd-core --test repx_aggregate -- --ignored
```

输出目录必须尚不存在；省略 `--output` 时使用 `output/repx-aggregate-validation` 下的时间戳目录。目录保存捕获、SDK getter 真值、参考与导出 XML、导入结果及 `result.json`。`--producer-only` 仅检查官方 SDK 参考 XML 的往返，不验证 Rust 导出。

## 边界

只验证 PhysX 3.4.2 的刚体 Aggregate。包含 Articulation 的组不在导出范围内。成员关系取自捕获中的 `Aggregate.Actors`；SDK 的 PVD 属性过滤器不发送 `Actor.Aggregate`。

选中导出沿 Aggregate 成员和关节端点递归扩展。互验检查这一闭包能否由 SDK 导入并恢复相同关系；原版 PVD3 对应入口的具体范围仍需独立对照。完整导出限制见 [RepX 文档](../../docs/repx.md)。
