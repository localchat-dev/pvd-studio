# CPU Cloth 的 RepX 数据边界

此工具核对 PhysX 3.4.2 PVD 捕获是否包含官方 RepX 所需的 Fabric tether 数据，不提供 Cloth 导出。

同一次运行创建 4×5 CPU Cloth，保存 PVD 流、Fabric getter 真值与官方参考 XML；官方 importer 重读 XML 后再次核对 getter。随后读取 PVD 帧，检查 Fabric 是否包含 `NbTethers`、`TetherAnchors` 和 `TetherLengths`。

## 运行

按 [SDK 生产器说明](../physx-sample/README.md)准备固定 SDK commit `5e42a5f112351a223c19c17bb331e6c55037b8eb`，在仓库根目录执行：

```powershell
.\tools\repx-cloth\build.ps1
cargo build --locked -p pvd-cli
python tools/repx-cloth/verify.py --output output/repx-cloth-check
```

输出目录必须尚不存在。省略 `--output` 时在 `output/repx-cloth-validation` 下创建时间戳目录。捕获、参考 XML、getter 真值、帧属性和结论写入该目录。

## 数据限制

官方 `SnRepXCoreSerializer.cpp` 的 Fabric serializer 保存 tether anchors 和 lengths，importer 用它们创建 Fabric。PVD `PxClothFabricGeneratedInfo` 与 `PvdMetaDataBinding::sendAllProperties(PxClothFabric)` 没有发送这两组数据，也没有发送 tether 数量。样例 getter 与官方 XML 均包含非空 tether，而同源 PVD 属性缺失。

因此不能把缺失属性解释为空 tether，也不能从当前粒子或约束图猜测烘焙结果。需要来源额外提供完整 tether 数据，才能恢复这种 Fabric。

`previousParticles` 属于另一项边界：官方 Cloth RepX 也只保存当前粒子，不保存求解器历史；它并不能证明 Fabric tether 可以省略。
