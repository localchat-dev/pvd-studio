# RepX 共享 Shape

共享 Shape 导出为一个顶层 `PxShape`，各 Actor 通过 `Shapes/PxShapeRef` 引用。

## 格式与所有权

固定 PhysX 3.4.2 SDK commit `5e42a5f112351a223c19c17bb331e6c55037b8eb` 的 `SnXmlVisitorWriter.h` 和 `SnXmlVisitorReader.h` 定义上述独立对象与引用形式。`SnRepXCoreSerializer.cpp` 以非 exclusive 模式创建顶层 Shape，Actor 导入时附着同一对象。

SDK `PvdMetaDataPvdBinding.cpp:1045–1153` 仅在首次创建捕获 Shape 时写入 `Shape.Actor`；后续附着、解除附着不会将它重新绑定到剩余 Actor。因此共享 Shape 的 `Actor` 字段可能指向已经删除的首个拥有者。导出依赖以当前代际有效的 `Actor.Shapes` 与 `PxPhysics.SharedShapes` 为准，历史 Actor 提示只检查字段类型。exclusive Shape 仍要求唯一且有效的所属 Actor。

选中 Actor 导出只保留所选 Actor 的附着关系，不引入其他共享者；共享 Shape 仍保持非 exclusive。整帧导出按唯一 Shape 计数，保留各 Actor 的 Shape 数组顺序。几何限制对所有选中拥有者生效，存在动态拥有者时不能借静态拥有者绕过限制。

## 实际验证

独立官方 SDK 生产器覆盖五个完整帧：双 Actor 共享、首个拥有者解除附着并删除、新 Actor 附着、全部解除后修改并重建捕获代际、共享与 exclusive Shape 混合。5 份 SDK 自写参考 XML 与 14 份候选导出均由官方 `createCollectionFromXml` 重新读取。

getter 对照验证了导入进程内多个 Actor 持有相同 Shape 指针、`isExclusive=false`、附着集合、Shape 数组顺序、Actor/Shape 位姿与 flags、过滤数据、偏移、名称、几何及有序材质。并非只比较几何相同。共享 ID 在前 3 帧为代际 0、后 2 帧为代际 1；首个 Actor 删除后捕获的 `Shape.Actor` 仍指向旧 ID，导出没有重新引入它。

另外 3 项需真实输入的测试覆盖错误字段、重复引用、失效代际、缺失/重复 Physics 注册、选中闭包及每个拥有者的几何限制。将真实状态改为共享 plane 的用例仅验证拒绝边界，不是共享 plane 的独立 SDK 兼容证据。

使用同次 SDK 捕获和 getter 真值检查共享身份、生命周期、选中闭包，再由官方 importer 重读导出结果。

## 重复验证

从项目根目录执行，每次验证使用新的输出目录。SDK checkout 需要先按项目既有流程构建。

```powershell
$physxRoot = (Resolve-Path '.deps/physx-3.4').Path
cmake -S tools/repx-shared-shapes -B output/repx-shared-build -G 'Visual Studio 18 2026' -A x64 "-DPHYSX34_ROOT=$physxRoot"
cmake --build output/repx-shared-build --config Release --target pvd-repx-shared-shapes --parallel 2
cargo build --locked --release -p pvd-cli
python tools/repx-shared-shapes/verify.py --producer output/repx-shared-build/Release/pvd-repx-shared-shapes.exe --cli target/release/pvd-cli.exe --output output/repx-shared-check
$env:PVD_REPX_SHARED_CAPTURE='output/repx-shared-check/shared.pvdstream'
cargo test --locked -p pvd-core --lib repx::shared_tests -- --ignored --nocapture
```

范围是 PhysX 3.4.2 的上述共享基础 Shape 场景；其他共享几何组合、SDK 版本、Articulation/Aggregate 与原版 PVD3 RepX 界面行为仍未验收。已有 128 MiB 上限约束网格 XML，不是整体 XML 或进程内存上限。RepX 导入不用于重放捕获或重新模拟展示。
