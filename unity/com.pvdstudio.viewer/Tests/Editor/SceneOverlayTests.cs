using System.IO;
using NUnit.Framework;
using UnityEditor.SceneManagement;
using UnityEngine;
using UnityEngine.Rendering;
using UnityEngine.SceneManagement;

namespace PvdStudio.Editor.Tests
{
    public class SceneOverlayTests
    {
        private static byte[] Frame(ulong room = 1, uint actorGeneration = 1, uint shapeGeneration = 1,
            float actorX = 2, float halfExtent = 1, byte state = 0, bool empty = false, ulong actorId = 101)
        {
            using (var stream = new MemoryStream())
            using (var writer = new BinaryWriter(stream))
            {
                writer.Write(0x31535650u); // PVS1
                writer.Write(empty ? 0u : 1u);
                if (!empty)
                {
                    writer.Write(room);
                    writer.Write(actorId);
                    writer.Write(actorGeneration);
                    writer.Write(202ul);
                    writer.Write(shapeGeneration);
                    writer.Write(state);
                    writer.Write((byte)0);
                    foreach (float x in new[] { actorX, 3f })
                        foreach (float value in new[] { x, 0, 0, 0, 0, 0, 1 })
                            writer.Write(value);
                    writer.Write(halfExtent);
                    writer.Write(1f);
                    writer.Write(1f);
                }
                return stream.ToArray();
            }
        }

        private static Transform Shape => SceneOverlay.Root.transform.GetChild(0).GetChild(0).GetChild(0);

        [TearDown]
        public void TearDown()
        {
            SceneOverlay.Clear();
            SceneOverlay.MirrorX = false;
            SceneOverlay.Wireframe = false;
            SceneOverlay.ShowInGameView = false;
        }

        [Test]
        public void MotionAndRoomChangesPreserveNodesAndMesh()
        {
            SceneOverlay.Show(Frame());
            var shape = Shape;
            var actor = shape.parent;
            var oldRoom = actor.parent;
            var filter = shape.GetComponent<MeshFilter>();
            var renderer = shape.GetComponent<MeshRenderer>();
            var mesh = filter.sharedMesh;
            var material = renderer.sharedMaterial;
            Assert.That(mesh.triangles.Length, Is.GreaterThan(0));
            Assert.IsNull(shape.GetComponent<Collider>());
            var levels = SceneOverlay.Root.GetComponent<LODGroup>().GetLODs();
            Assert.AreEqual(2, levels.Length);
            Assert.AreEqual(1, levels[0].renderers.Length);
            Assert.AreSame(renderer, levels[0].renderers[0]);
            Assert.AreEqual(0, levels[1].renderers.Length);
            for (var node = shape; node != null; node = node.parent)
            {
                Assert.AreNotEqual(0, (int)(node.gameObject.hideFlags & HideFlags.DontSaveInEditor));
                Assert.AreNotEqual(0, (int)(node.gameObject.hideFlags & HideFlags.DontSaveInBuild));
                Assert.AreEqual(0, (int)(node.gameObject.hideFlags & HideFlags.HideInHierarchy));
            }

            SceneOverlay.Show(Frame(room: 2, actorX: 7, state: 1));
            Assert.AreSame(shape, Shape);
            Assert.AreSame(actor, Shape.parent);
            Assert.IsTrue(oldRoom == null);
            Assert.AreSame(mesh, filter.sharedMesh);
            Assert.AreNotSame(material, renderer.sharedMaterial);
            Assert.AreEqual(7, actor.localPosition.x);
            Assert.AreEqual(3, shape.localPosition.x);
            SceneOverlay.MirrorX = true;
            Assert.AreEqual(-10, shape.position.x);
            SceneOverlay.Show(Frame(halfExtent: 4));
            Assert.AreSame(shape, Shape);
            Assert.IsTrue(mesh == null);
            Assert.AreEqual(8, filter.sharedMesh.bounds.size.x);
        }

        [Test]
        public void CropAnchorUsesActorIdentityForRenamedActorAndShape()
        {
            SceneOverlay.Show(Frame(actorGeneration: 4, shapeGeneration: 8));
            var shape = Shape.gameObject;
            var actor = Shape.parent.gameObject;
            shape.name = "Renamed shape";
            actor.name = "Renamed actor";
            Assert.IsTrue(SceneOverlay.TryGetActor(shape, out var shapeActorId, out var shapeActorGeneration));
            Assert.IsTrue(SceneOverlay.TryGetActor(actor, out var actorId, out var actorGeneration));
            Assert.AreEqual(101ul, actorId);
            Assert.AreEqual(4u, actorGeneration);
            Assert.AreEqual(actorId, shapeActorId);
            Assert.AreEqual(actorGeneration, shapeActorGeneration);
            Assert.IsTrue(!SceneOverlay.TryGetActor(SceneOverlay.Root, out _, out _));

            SceneOverlay.Show(Frame(actorGeneration: 5));
            Assert.IsTrue(!SceneOverlay.TryGetActor(actor, out _, out _));
            Assert.IsTrue(SceneOverlay.TryGetActor(Shape.gameObject, out var replacementId, out var replacementGeneration));
            Assert.AreEqual(actorId, replacementId);
            Assert.AreEqual(5u, replacementGeneration);
        }

        [Test]
        public void WireframeSwitchesDisplayedShapesWithoutRequestingAnotherFrame()
        {
            SceneOverlay.Show(Frame());
            var shape = Shape;
            var filter = shape.GetComponent<MeshFilter>();
            var renderer = shape.GetComponent<MeshRenderer>();
            var solid = filter.sharedMesh;
            var solidMaterial = renderer.sharedMaterial;

            SceneOverlay.Wireframe = true;
            var wire = filter.sharedMesh;
            var wireMaterial = renderer.sharedMaterial;
            Assert.AreEqual(MeshTopology.Lines, wire.GetTopology(0));
            Assert.AreEqual(24, wire.GetIndices(0).Length);
            Assert.AreNotSame(solidMaterial, wireMaterial);
            Assert.AreSame(shape, Shape);
            var first = SurfaceFrame.Decode(Frame(actorX: 9))[0];
            var second = SurfaceFrame.Decode(Frame(actorId: 102))[0];
            SceneOverlay.Show(new[] { first, second });
            var other = shape.parent.parent.GetChild(1).GetChild(0).GetComponent<MeshFilter>();
            Assert.AreSame(wire, other.sharedMesh);
            Assert.AreSame(wire, filter.sharedMesh);

            SceneOverlay.Wireframe = false;
            Assert.AreSame(solid, filter.sharedMesh);
            Assert.AreSame(solid, other.sharedMesh);
            Assert.AreSame(solidMaterial, renderer.sharedMaterial);
            SceneOverlay.Wireframe = true;
            Assert.AreSame(wire, filter.sharedMesh);
            Assert.AreSame(wireMaterial, renderer.sharedMaterial);
            SceneOverlay.Clear();
            Assert.IsTrue(wire == null && solid == null && wireMaterial == null && solidMaterial == null);
        }

        [Test]
        public void RaysBatchRecordedSegmentsAndHitMarkersUnderTheMirroredRoot()
        {
            var start = new Vector3(2, 1, 0);
            var end = new Vector3(4, 1, 0);
            var hit = new Vector3(3, 2, 1);
            SceneOverlay.ShowRays(new[]
            {
                new RayRecord(start, end, Vector3.zero, false, false),
                new RayRecord(Vector3.zero, hit, hit, true, true)
            });

            Assert.AreEqual(1, SceneOverlay.Root.transform.childCount);
            var batch = SceneOverlay.Root.transform.GetChild(0);
            var renderer = batch.GetComponent<MeshRenderer>();
            var mesh = batch.GetComponent<MeshFilter>().sharedMesh;
            Assert.AreEqual(2, mesh.subMeshCount);
            Assert.AreEqual(MeshTopology.Lines, mesh.GetTopology(0));
            Assert.AreEqual(MeshTopology.Lines, mesh.GetTopology(1));
            Assert.AreNotEqual(renderer.sharedMaterials[0].color, renderer.sharedMaterials[1].color);
            Assert.AreNotEqual(0, (int)(batch.gameObject.hideFlags & HideFlags.HideInHierarchy));
            Assert.IsTrue(SceneOverlay.IsContainer(batch.gameObject));
            Assert.IsFalse(SceneOverlay.TryGetActor(batch.gameObject, out _, out _));

            var vertices = mesh.vertices;
            var hits = mesh.GetIndices(0);
            var misses = mesh.GetIndices(1);
            Assert.AreEqual(8, hits.Length);
            Assert.AreEqual(2, misses.Length);
            Assert.AreEqual(Vector3.zero, vertices[hits[0]]);
            Assert.AreEqual(hit, vertices[hits[1]]);
            Assert.AreEqual(start, vertices[misses[0]]);
            Assert.AreEqual(end, vertices[misses[1]]);
            for (int i = 2; i < hits.Length; i += 2)
            {
                Assert.AreNotEqual(vertices[hits[i]], vertices[hits[i + 1]]);
                Assert.AreEqual(hit, (vertices[hits[i]] + vertices[hits[i + 1]]) * 0.5f);
            }

            var levels = SceneOverlay.Root.GetComponent<LODGroup>().GetLODs();
            Assert.AreEqual(1, levels[0].renderers.Length);
            Assert.AreSame(renderer, levels[0].renderers[0]);
            Assert.AreEqual(0, levels[1].renderers.Length);
            SceneOverlay.MirrorX = true;
            Assert.AreEqual(new Vector3(-4, 1, 0), batch.TransformPoint(vertices[misses[1]]));
        }

        [Test]
        public void IdenticalRayFramesDoNotUpdateTheOverlay()
        {
            var record = new RayRecord(Vector3.zero, Vector3.one, Vector3.zero, false, false);
            SceneOverlay.ShowRays(new[] { record });
            var batch = SceneOverlay.Root.transform.GetChild(0);
            var mesh = batch.GetComponent<MeshFilter>().sharedMesh;
            Assert.IsFalse(SceneOverlay.ShowRays(new[] { record }));
            Assert.AreSame(mesh, batch.GetComponent<MeshFilter>().sharedMesh);

            var hit = new RayRecord(Vector3.zero, Vector3.one, Vector3.one, true, true);
            Assert.IsTrue(SceneOverlay.ShowRays(new[] { hit }));
            Assert.AreSame(mesh, batch.GetComponent<MeshFilter>().sharedMesh);
            Assert.AreEqual(8, mesh.GetIndices(0).Length);
        }

        [Test]
        public void EmptyAndDisabledRayFramesClearTheBatchAndPreserveSurfaces()
        {
            SceneOverlay.Show(Frame());
            var shapeRenderer = Shape.GetComponent<MeshRenderer>();
            var records = new[] { new RayRecord(Vector3.zero, Vector3.one, Vector3.zero, false, false) };
            SceneOverlay.ShowRays(records);
            var batch = SceneOverlay.Root.transform.GetChild(1);
            var renderer = batch.GetComponent<MeshRenderer>();
            var mesh = batch.GetComponent<MeshFilter>().sharedMesh;
            var material = renderer.sharedMaterials[1];
            var group = SceneOverlay.Root.GetComponent<LODGroup>();
            Assert.AreEqual(2, group.GetLODs()[0].renderers.Length);

            SceneOverlay.ShowRays(System.Array.Empty<RayRecord>());
            Assert.IsFalse(renderer.enabled);
            Assert.AreEqual(0, mesh.vertexCount);
            SceneOverlay.ShowRays(records);
            Assert.IsTrue(renderer.enabled);
            Assert.AreSame(mesh, batch.GetComponent<MeshFilter>().sharedMesh);
            Assert.AreEqual(2, mesh.GetIndices(1).Length);

            SceneOverlay.ShowRays(null);
            Assert.IsTrue(batch == null && mesh == null && material == null);
            Assert.AreEqual(1, group.GetLODs()[0].renderers.Length);
            Assert.AreSame(shapeRenderer, group.GetLODs()[0].renderers[0]);

            SceneOverlay.ShowRays(records);
            batch = SceneOverlay.Root.transform.GetChild(1);
            mesh = batch.GetComponent<MeshFilter>().sharedMesh;
            material = batch.GetComponent<MeshRenderer>().sharedMaterials[1];
            SceneOverlay.Clear();
            Assert.IsTrue(batch == null && mesh == null && material == null);
        }

        [Test]
        public void SweepAndOverlapLinesUseOneStyledMeshUnderTheMirroredRoot()
        {
            var lines = new[]
            {
                new QueryLine(Vector3.zero, new Vector3(1, 0, 0), 0),
                new QueryLine(Vector3.one, new Vector3(2, 1, 0), 1),
                new QueryLine(new Vector3(1, 2, 3), new Vector3(3, 2, 1), 2)
            };
            Assert.IsTrue(SceneOverlay.ShowQueries(lines));
            Assert.AreEqual(1, SceneOverlay.Root.transform.childCount);
            var batch = SceneOverlay.Root.transform.GetChild(0);
            var renderer = batch.GetComponent<MeshRenderer>();
            var mesh = batch.GetComponent<MeshFilter>().sharedMesh;
            Assert.AreEqual(3, mesh.subMeshCount);
            Assert.AreEqual(6, mesh.vertexCount);
            Assert.AreNotEqual(0, (int)(batch.gameObject.hideFlags & HideFlags.HideInHierarchy));
            Assert.IsTrue(SceneOverlay.IsContainer(batch.gameObject));
            Assert.IsFalse(SceneOverlay.TryGetActor(batch.gameObject, out _, out _));

            var vertices = mesh.vertices;
            var materials = renderer.sharedMaterials;
            for (int i = 0; i < lines.Length; i++)
            {
                Assert.AreEqual(MeshTopology.Lines, mesh.GetTopology(i));
                var indices = mesh.GetIndices(i);
                Assert.AreEqual(2, indices.Length);
                Assert.AreEqual(lines[i].Start, vertices[indices[0]]);
                Assert.AreEqual(lines[i].End, vertices[indices[1]]);
                Assert.AreNotEqual(materials[i].color, materials[(i + 1) % materials.Length].color);
            }
            var levels = SceneOverlay.Root.GetComponent<LODGroup>().GetLODs();
            Assert.AreEqual(1, levels[0].renderers.Length);
            Assert.AreSame(renderer, levels[0].renderers[0]);
            Assert.AreEqual(0, levels[1].renderers.Length);
            SceneOverlay.MirrorX = true;
            Assert.AreEqual(new Vector3(-3, 2, 1), batch.TransformPoint(vertices[mesh.GetIndices(2)[1]]));
        }

        [Test]
        public void IdenticalQueryLinesDoNotUpdateTheOverlay()
        {
            var line = new QueryLine(Vector3.zero, Vector3.one, 0);
            Assert.IsTrue(SceneOverlay.ShowQueries(new[] { line }));
            var batch = SceneOverlay.Root.transform.GetChild(0);
            var filter = batch.GetComponent<MeshFilter>();
            var mesh = filter.sharedMesh;
            Assert.IsFalse(SceneOverlay.ShowQueries(new[] { line }));

            var hitLine = new QueryLine(line.Start, line.End, 2);
            Assert.IsTrue(SceneOverlay.ShowQueries(new[] { hitLine }));
            Assert.AreSame(mesh, filter.sharedMesh);
            Assert.AreEqual(0, mesh.GetIndices(0).Length);
            Assert.AreEqual(2, mesh.GetIndices(2).Length);
        }

        [Test]
        public void EmptyAndDisabledQueryLinesPreserveSurfacesAndRays()
        {
            SceneOverlay.Show(Frame());
            var shapeRenderer = Shape.GetComponent<MeshRenderer>();
            SceneOverlay.ShowRays(new[] { new RayRecord(Vector3.zero, Vector3.one, Vector3.zero, false, false) });
            var rayRenderer = SceneOverlay.Root.transform.GetChild(1).GetComponent<MeshRenderer>();
            var lines = new[] { new QueryLine(Vector3.zero, Vector3.one, 0) };
            var group = SceneOverlay.Root.GetComponent<LODGroup>();
            SceneOverlay.ShowQueries(lines);
            var batch = SceneOverlay.Root.transform.GetChild(2);
            var renderer = batch.GetComponent<MeshRenderer>();
            var mesh = batch.GetComponent<MeshFilter>().sharedMesh;
            var material = renderer.sharedMaterials[0];
            Assert.AreEqual(3, group.GetLODs()[0].renderers.Length);

            Assert.IsTrue(SceneOverlay.ShowQueries(System.Array.Empty<QueryLine>()));
            Assert.IsFalse(SceneOverlay.ShowQueries(System.Array.Empty<QueryLine>()));
            Assert.IsFalse(renderer.enabled);
            Assert.AreEqual(0, mesh.vertexCount);
            Assert.IsTrue(SceneOverlay.ShowQueries(lines));
            Assert.AreSame(mesh, batch.GetComponent<MeshFilter>().sharedMesh);
            Assert.IsTrue(renderer.enabled);

            Assert.IsTrue(SceneOverlay.ShowQueries(null));
            Assert.IsTrue(batch == null && mesh == null && material == null);
            var members = group.GetLODs()[0].renderers;
            Assert.AreEqual(2, members.Length);
            Assert.AreSame(shapeRenderer, members[0]);
            Assert.AreSame(rayRenderer, members[1]);

            SceneOverlay.ShowQueries(lines);
            batch = SceneOverlay.Root.transform.GetChild(2);
            mesh = batch.GetComponent<MeshFilter>().sharedMesh;
            material = batch.GetComponent<MeshRenderer>().sharedMaterials[0];
            SceneOverlay.Clear();
            Assert.IsTrue(batch == null && mesh == null && material == null);
        }

        [Test]
        public void QueryMeshKeepsItsVertexCountWithinTheLineBudget()
        {
            var lines = new QueryLine[8193];
            for (int i = 0; i < lines.Length; i++)
                lines[i] = new QueryLine(Vector3.zero, new Vector3(i + 1, 1, 0), 0);
            Assert.IsTrue(SceneOverlay.ShowQueries(lines));
            var mesh = SceneOverlay.Root.transform.GetChild(0).GetComponent<MeshFilter>().sharedMesh;
            Assert.AreEqual(IndexFormat.UInt16, mesh.indexFormat);
            Assert.AreEqual(16384, mesh.vertexCount);
            Assert.AreEqual(new Vector3(8192, 1, 0), mesh.vertices[16383]);
        }

        [Test]
        public void StationaryRecordsKeepTransformsCleanAndRepairReparentedNodes()
        {
            var records = SurfaceFrame.Decode(Frame());
            SceneOverlay.Show(records);
            var shape = Shape;
            var actor = shape.parent;
            var room = actor.parent;
            shape.hasChanged = false;
            actor.hasChanged = false;
            SceneOverlay.Show(records);
            Assert.IsTrue(!actor.hasChanged && !shape.hasChanged);

            room.SetParent(null, false);
            actor.SetParent(null, false);
            shape.SetParent(null, false);
            SceneOverlay.Show(records);
            Assert.AreSame(shape, Shape);
            Assert.AreSame(actor, shape.parent);
            Assert.AreSame(room, actor.parent);

            records[0].ActorPosition = new Vector3(2.000001f, 0, 0);
            SceneOverlay.Show(records);
            Assert.AreEqual(records[0].ActorPosition.x, actor.localPosition.x);
        }

        [Test]
        public void GameViewOptInRendersTheSameSurfaceAndWireframeAsSceneView()
        {
            if (GraphicsSettings.currentRenderPipeline != null ||
                SystemInfo.graphicsDeviceType == GraphicsDeviceType.Null)
                Assert.Ignore("Requires the built-in render pipeline and a graphics device.");

            var preview = EditorSceneManager.NewPreviewScene();
            var previousTarget = RenderTexture.active;
            var target = RenderTexture.GetTemporary(32, 32, 24);
            var pixels = new Texture2D(32, 32);
            var material = new Material(Shader.Find("Unlit/Color")) { color = Color.red };
            try
            {
                SceneOverlay.Show(Frame());
                SceneManager.MoveGameObjectToScene(SceneOverlay.Root, preview);
                Shape.GetComponent<MeshRenderer>().sharedMaterial = material;
                var cameraNode = new GameObject("Overlay test camera");
                SceneManager.MoveGameObjectToScene(cameraNode, preview);
                var camera = cameraNode.AddComponent<Camera>();
                camera.enabled = false;
                camera.overrideSceneCullingMask = EditorSceneManager.GetSceneCullingMask(preview);
                camera.targetTexture = target;
                camera.clearFlags = CameraClearFlags.SolidColor;
                camera.backgroundColor = Color.blue;
                camera.transform.position = new Vector3(5, 0, -6);

                Color Sample(CameraType type)
                {
                    camera.cameraType = type;
                    camera.Render();
                    RenderTexture.active = target;
                    pixels.ReadPixels(new Rect(0, 0, 32, 32), 0, 0);
                    return pixels.GetPixel(16, 16);
                }

                Assert.IsTrue(Sample(CameraType.Game).b > 0.8f);
                Assert.IsTrue(Sample(CameraType.SceneView).r > 0.8f);
                Assert.IsTrue(Sample(CameraType.Game).b > 0.8f);
                SceneOverlay.ShowInGameView = true;
                Assert.IsTrue(Sample(CameraType.Game).r > 0.8f);
                SceneOverlay.Wireframe = true;
                Assert.IsTrue(Sample(CameraType.SceneView).r < 0.05f);
                Assert.IsTrue(System.Array.Exists(pixels.GetPixels(), color => color.r > 0.1f));
                Sample(CameraType.Game);
                Assert.IsTrue(System.Array.Exists(pixels.GetPixels(), color => color.r > 0.1f));
                Assert.IsTrue(pixels.GetPixel(16, 16).r < 0.05f);
                SceneOverlay.ShowInGameView = false;
                Sample(CameraType.Game);
                Assert.IsTrue(System.Array.TrueForAll(pixels.GetPixels(), color => color.r < 0.05f));
                Sample(CameraType.SceneView);
                Assert.IsTrue(System.Array.Exists(pixels.GetPixels(), color => color.r > 0.1f));
            }
            finally
            {
                RenderTexture.active = previousTarget;
                SceneOverlay.Clear();
                EditorSceneManager.ClosePreviewScene(preview);
                RenderTexture.ReleaseTemporary(target);
                Object.DestroyImmediate(material);
                Object.DestroyImmediate(pixels);
            }
        }

        [Test]
        public void SeekAndReusedIdsReleaseOldInstances()
        {
            SceneOverlay.Show(Frame());
            var first = Shape;
            var firstActor = first.parent;
            SceneOverlay.Show(Frame(shapeGeneration: 2));
            Assert.IsTrue(first == null);
            Assert.AreSame(firstActor, Shape.parent);
            SceneOverlay.Show(Frame(actorGeneration: 2));
            Assert.IsTrue(firstActor == null);
            SceneOverlay.Show(Frame(empty: true));
            Assert.AreEqual(0, SceneOverlay.Root.transform.childCount);
            SceneOverlay.Show(Frame());
            Assert.AreEqual(5, Shape.position.x);
        }

        [Test]
        public void SharedShapeHasIndependentNodesAndOneMeshAcrossActors()
        {
            var firstFrame = Frame();
            var secondFrame = Frame(actorId: 102);
            using (var stream = new MemoryStream())
            using (var writer = new BinaryWriter(stream))
            {
                writer.Write(0x31535650u);
                writer.Write(2u);
                writer.Write(firstFrame, 8, firstFrame.Length - 8);
                writer.Write(secondFrame, 8, secondFrame.Length - 8);
                SceneOverlay.Show(stream.ToArray());
            }
            var room = SceneOverlay.Root.transform.GetChild(0);
            var firstShape = room.GetChild(0).GetChild(0);
            var secondShape = room.GetChild(1).GetChild(0);
            var shared = firstShape.GetComponent<MeshFilter>().sharedMesh;
            Assert.AreSame(shared, secondShape.GetComponent<MeshFilter>().sharedMesh);
            SceneOverlay.Show(secondFrame);
            Assert.IsTrue(firstShape == null);
            Assert.AreSame(secondShape, Shape);
            Assert.IsTrue(shared != null);
            SceneOverlay.Show(Frame(empty: true));
            Assert.IsTrue(shared == null);
        }

        [Test]
        public void ClearPreservesUserChildrenAndReleasesOwnedResources()
        {
            SceneOverlay.Show(Frame());
            var userObject = new GameObject("User object");
            try
            {
                var shape = Shape;
                var mesh = shape.GetComponent<MeshFilter>().sharedMesh;
                var material = shape.GetComponent<MeshRenderer>().sharedMaterial;
                userObject.transform.SetParent(shape, false);
                var position = userObject.transform.position;
                SceneOverlay.Clear();
                SceneOverlay.Clear();
                Assert.IsTrue(SceneOverlay.Root == null);
                Assert.IsTrue(shape == null && mesh == null && material == null);
                Assert.IsTrue(userObject != null);
                Assert.IsNull(userObject.transform.parent);
                Assert.AreEqual(position, userObject.transform.position);
            }
            finally
            {
                if (userObject != null) Object.DestroyImmediate(userObject);
            }
        }
    }
}
