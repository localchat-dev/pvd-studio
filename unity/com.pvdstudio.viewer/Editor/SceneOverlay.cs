using System.Collections.Generic;
using UnityEditor;
using UnityEngine;
using UnityEngine.Rendering;
using Object = UnityEngine.Object;

namespace PvdStudio.Editor
{
    internal static class SceneOverlay
    {
        private const HideFlags NodeFlags = HideFlags.DontSaveInEditor | HideFlags.DontSaveInBuild;
        private static readonly Color[] Colors =
        {
            new Color(0.42f, 0.81f, 0.93f),
            new Color(1.00f, 0.70f, 0.27f),
            new Color(0.51f, 0.82f, 0.55f)
        };

        private sealed class SharedMesh
        {
            internal SurfaceGeometry Geometry;
            internal int Users;
            private Mesh solid, wire;

            internal Mesh GetMesh(bool wireframe)
            {
                if (wireframe)
                {
                    if (wire == null) wire = Geometry.BuildWireMesh();
                    return wire;
                }
                if (solid == null) solid = Geometry.BuildMesh();
                return solid;
            }

            internal void DestroyMeshes()
            {
                if (wire != null) Object.DestroyImmediate(wire);
                if (solid != null) Object.DestroyImmediate(solid);
            }
        }

        private class OverlayNode
        {
            internal GameObject Node;
            internal Transform Transform, Parent;
            internal ulong Seen;
            internal Vector3 Position;
            internal Quaternion Rotation;
        }

        private sealed class Shape : OverlayNode
        {
            internal MeshFilter Filter;
            internal MeshRenderer Renderer;
            internal Material Material;
            internal byte State;
            internal SharedMesh Surface;
        }

        private abstract class LineBatch<T>
        {
            internal readonly GameObject Node;
            internal readonly MeshRenderer Renderer;
            private readonly MeshFilter filter;
            private readonly Mesh mesh;
            private readonly (string Name, Color Color)[] palette;
            private readonly Material[] materials;
            private readonly List<int>[] indices;
            private readonly List<Vector3> vertices = new List<Vector3>();
            private readonly int maxRecords;
            private T[] displayed;

            protected LineBatch(Transform parent, string name, (string Name, Color Color)[] palette,
                int maxRecords = int.MaxValue)
            {
                this.palette = palette;
                this.maxRecords = maxRecords;
                materials = new Material[palette.Length];
                indices = new List<int>[palette.Length];
                for (int i = 0; i < indices.Length; i++) indices[i] = new List<int>();
                Node = CreateNode(name, parent, NodeFlags | HideFlags.HideInHierarchy);
                try
                {
                    filter = Node.AddComponent<MeshFilter>();
                    Renderer = CreateRenderer(Node);
                    mesh = new Mesh { name = name, hideFlags = HideFlags.HideAndDontSave };
                    mesh.MarkDynamic();
                    filter.sharedMesh = mesh;
                    RefreshMaterials();
                }
                catch
                {
                    Destroy();
                    throw;
                }
            }

            internal bool Exists => Node != null && filter != null && Renderer != null && mesh != null;

            internal bool Show(T[] records)
            {
                int count = System.Math.Min(records.Length, maxRecords);
                if (Renderer.enabled && MatchesDisplayed(records, count)) return false;
                vertices.Clear();
                foreach (var group in indices) group.Clear();
                for (int i = 0; i < count; i++) AddRecord(records[i]);
                mesh.Clear();
                mesh.SetVertices(vertices);
                mesh.subMeshCount = indices.Length;
                for (int i = 0; i < indices.Length; i++)
                    mesh.SetIndices(indices[i], MeshTopology.Lines, i, false);
                mesh.RecalculateBounds();
                Renderer.enabled = true;
                displayed = records;
                return true;
            }

            private bool MatchesDisplayed(T[] records, int count)
            {
                if (ReferenceEquals(displayed, records)) return true;
                if (displayed == null || System.Math.Min(displayed.Length, maxRecords) != count) return false;
                for (int i = 0; i < count; i++)
                    if (!RecordsMatch(displayed[i], records[i])) return false;
                return true;
            }

            protected abstract bool RecordsMatch(T previous, T next);
            protected abstract void AddRecord(T record);

            protected void AddLine(int style, Vector3 start, Vector3 end)
            {
                var group = indices[style];
                group.Add(vertices.Count);
                vertices.Add(start);
                group.Add(vertices.Count);
                vertices.Add(end);
            }

            internal bool Hide()
            {
                if (Renderer == null || !Renderer.enabled) return false;
                Renderer.enabled = false;
                displayed = null;
                if (mesh != null) mesh.Clear();
                return true;
            }

            internal void RefreshMaterials()
            {
                var pipeline = GraphicsSettings.currentRenderPipeline;
                for (int i = 0; i < materials.Length; i++)
                {
                    if (materials[i] != null) Object.DestroyImmediate(materials[i]);
                    var material = materials[i] = CreateWireMaterial(pipeline);
                    material.name = palette[i].Name;
                    material.hideFlags = HideFlags.HideAndDontSave;
                    material.color = palette[i].Color;
                }
                Renderer.sharedMaterials = materials;
            }

            internal void Destroy()
            {
                DestroyNode(Node);
                if (mesh != null) Object.DestroyImmediate(mesh);
                foreach (var material in materials)
                    if (material != null) Object.DestroyImmediate(material);
            }
        }

        private sealed class RayBatch : LineBatch<RayRecord>
        {
            private static readonly (string, Color)[] Palette =
            {
                ("PVD Raycast Hit", new Color(1f, 0.32f, 0.24f)),
                ("PVD Raycast Miss", new Color(0.40f, 0.92f, 1f))
            };
            private static readonly Vector3[] MarkerOffsets =
            {
                new Vector3(0.12f, 0, 0), new Vector3(0, 0.12f, 0), new Vector3(0, 0, 0.12f)
            };

            internal RayBatch(Transform parent) : base(parent, "PVD Raycasts", Palette) { }

            protected override bool RecordsMatch(RayRecord previous, RayRecord next) =>
                previous.Hit == next.Hit && previous.HasMarker == next.HasMarker &&
                previous.Start.Equals(next.Start) && previous.End.Equals(next.End) &&
                (!next.HasMarker || previous.Marker.Equals(next.Marker));

            protected override void AddRecord(RayRecord record)
            {
                AddLine(record.Hit ? 0 : 1, record.Start, record.End);
                if (!record.HasMarker) return;
                foreach (var offset in MarkerOffsets)
                    AddLine(0, record.Marker - offset, record.Marker + offset);
            }
        }

        private sealed class QueryBatch : LineBatch<QueryLine>
        {
            private static readonly (string, Color)[] Palette =
            {
                ("PVD Sweep", new Color(0.22f, 0.66f, 1f)),
                ("PVD Overlap", new Color(0.83f, 0.46f, 1f)),
                ("PVD Query Hit", new Color(1f, 0.91f, 0.20f))
            };

            internal QueryBatch(Transform parent)
                : base(parent, "PVD Sweeps and Overlaps", Palette, SurfaceFrameReader.MaxQueryLines) { }

            protected override bool RecordsMatch(QueryLine previous, QueryLine next) =>
                previous.Style == next.Style && previous.Start.Equals(next.Start) && previous.End.Equals(next.End);

            protected override void AddRecord(QueryLine line) => AddLine(line.Style, line.Start, line.End);
        }

        private static readonly Dictionary<ulong, OverlayNode> Rooms = new Dictionary<ulong, OverlayNode>();
        private static readonly Dictionary<(ulong ActorId, uint Generation), OverlayNode> Actors =
            new Dictionary<(ulong, uint), OverlayNode>();
        private static readonly Dictionary<(ulong ActorId, uint ActorGeneration, ulong ShapeId, uint ShapeGeneration), Shape> Shapes =
            new Dictionary<(ulong, uint, ulong, uint), Shape>();
        private static readonly Dictionary<SurfaceGeometry, SharedMesh> Meshes = new Dictionary<SurfaceGeometry, SharedMesh>();
        private static readonly Stack<bool> CameraVisibility = new Stack<bool>();
        private static readonly Material[] Materials = new Material[Colors.Length];
        private static readonly Material[] OpaqueMaterials = new Material[Colors.Length];
        private static readonly Material[] WireMaterials = new Material[Colors.Length];
        private static readonly List<ulong> RemovedRooms = new List<ulong>();
        private static readonly List<(ulong, uint)> RemovedActors = new List<(ulong, uint)>();
        private static readonly List<(ulong, uint, ulong, uint)> RemovedShapes = new List<(ulong, uint, ulong, uint)>();
        private static LODGroup visibilityGroup;
        private static LineBatch<RayRecord> rays;
        private static LineBatch<QueryLine> queries;
        private static ulong frameNumber;
        private static bool visible;
        private static bool subscribed;
        private static bool mirrorX;
        private static bool lostNode;
        private static int repaintDepth;
        private static bool repaintPending;

        internal readonly struct RepaintScope : System.IDisposable
        {
            public void Dispose()
            {
                repaintDepth--;
                if (repaintDepth != 0 || !repaintPending) return;
                repaintPending = false;
                RepaintViews();
            }
        }

        internal static RepaintScope BatchRepaints()
        {
            repaintDepth++;
            return new RepaintScope();
        }

        internal static GameObject Root { get; private set; }
        internal static bool NeedsFrame => Root == null || lostNode;

        internal static bool IsContainer(GameObject node)
        {
            if (node == null) return false;
            if (node == Root || (rays != null && node == rays.Node) || (queries != null && node == queries.Node))
                return true;
            foreach (var room in Rooms.Values)
                if (node == room.Node) return true;
            return false;
        }

        internal static bool TryGetActor(GameObject node, out ulong id, out uint generation)
        {
            if (node != null)
            {
                foreach (var entry in Actors)
                {
                    if (entry.Value.Node != node) continue;
                    (id, generation) = entry.Key;
                    return true;
                }
                foreach (var entry in Shapes)
                {
                    if (entry.Value.Node != node) continue;
                    id = entry.Key.ActorId;
                    generation = entry.Key.ActorGeneration;
                    return true;
                }
            }
            id = 0;
            generation = 0;
            return false;
        }

        private static bool showInGameView;
        internal static bool ShowInGameView
        {
            get => showInGameView;
            set
            {
                if (showInGameView == value) return;
                showInGameView = value;
                if (Root != null) RequestGameViewUpdate();
            }
        }

        private static void RequestGameViewUpdate()
        {
            // During playback the Game view already renders on the player loop.
            if (!EditorApplication.isPlaying || EditorApplication.isPaused)
                EditorApplication.QueuePlayerLoopUpdate();
        }

        private static void RepaintViews()
        {
            if (repaintDepth != 0)
            {
                repaintPending = true;
                return;
            }
            SceneView.RepaintAll();
            if (showInGameView) RequestGameViewUpdate();
        }

        private static bool wireframe;
        internal const float DefaultSurfaceOpacity = 0.16f;
        private static float surfaceOpacity = DefaultSurfaceOpacity;
        internal static float SurfaceOpacity
        {
            get => surfaceOpacity;
            set
            {
                float opacity = float.IsNaN(value) ? DefaultSurfaceOpacity : Mathf.Clamp01(value);
                if (surfaceOpacity == opacity) return;
                bool changeRenderers = (surfaceOpacity == 1f) != (opacity == 1f)
                    || (surfaceOpacity == 0f) != (opacity == 0f);
                surfaceOpacity = opacity;
                for (int i = 0; i < Materials.Length; i++)
                    if (Materials[i] != null) Materials[i].color = SurfaceTint(i);
                if (wireframe) return;
                if (changeRenderers) ApplyShapeAppearance(false);
                RepaintViews();
            }
        }

        private static Color SurfaceTint(int state)
        {
            var color = Colors[state];
            color.a = surfaceOpacity;
            return color;
        }

        internal static bool Wireframe
        {
            get => wireframe;
            set
            {
                if (wireframe == value) return;
                // Build the requested variants before replacing anything in the visible frame.
                foreach (var shared in Meshes.Values) shared.GetMesh(value);
                wireframe = value;
                ApplyShapeAppearance(true);
                RepaintViews();
            }
        }

        private static Material[] ActiveMaterials => wireframe ? WireMaterials
            : surfaceOpacity == 1f ? OpaqueMaterials : Materials;

        private static void ApplyShapeAppearance(bool updateMesh)
        {
            foreach (var shape in Shapes.Values)
            {
                if (shape.Filter == null || shape.Renderer == null || shape.Surface == null)
                {
                    lostNode = true;
                    continue;
                }
                if (updateMesh) shape.Filter.sharedMesh = shape.Surface.GetMesh(wireframe);
                ApplyShapeMaterial(shape);
            }
        }

        internal static bool MirrorX
        {
            get => mirrorX;
            set
            {
                if (mirrorX == value) return;
                mirrorX = value;
                if (Root != null) Root.transform.localScale = RootScale;
                RepaintViews();
            }
        }

        private static Vector3 RootScale => mirrorX ? new Vector3(-1, 1, 1) : Vector3.one;

        internal static void Show(byte[] frame) => Show(SurfaceFrame.Decode(frame));

        private static bool EnsureRoot()
        {
            bool changed = false;
            if (Root == null)
            {
                Clear();
                Root = CreateNode("PVD Studio", null);
                Root.transform.localScale = RootScale;
                Subscribe();
                changed = true;
            }
            if (visibilityGroup == null)
            {
                visibilityGroup = Root.AddComponent<LODGroup>();
                visibilityGroup.hideFlags = HideFlags.HideInInspector;
                changed = true;
            }
            if (Root.transform.parent != null)
            {
                Root.transform.SetParent(null, false);
                changed = true;
            }
            return changed;
        }

        internal static void Show(SurfaceRecord[] records)
        {
            bool changed = false;
            bool membersChanged = EnsureRoot();
            frameNumber++;
            int seenRooms = 0, seenActors = 0, seenShapes = 0;
            var rootTransform = Root.transform;
            foreach (var record in records)
            {
                var actorKey = (record.ActorId, record.ActorGeneration);
                var shapeKey = (record.ActorId, record.ActorGeneration, record.ShapeId, record.ShapeGeneration);
                if (!Rooms.TryGetValue(record.RoomNumber, out var room) || room.Node == null)
                {
                    string label = record.RoomNumber == 0 ? "Unassigned" : $"Physics Scene {record.RoomNumber}";
                    Rooms[record.RoomNumber] = room = CreateOverlayNode<OverlayNode>(label, rootTransform);
                    changed = true;
                }
                if (MarkSeen(room))
                {
                    seenRooms++;
                    changed |= SetParent(room, rootTransform);
                }

                if (!Actors.TryGetValue(actorKey, out var actor) || actor.Node == null)
                {
                    Actors[actorKey] = actor = CreateOverlayNode<OverlayNode>(
                        $"Actor {record.ActorId} [{record.ActorGeneration}]", room.Transform);
                    changed = true;
                }
                bool firstActorRecord = MarkSeen(actor);
                if (firstActorRecord) seenActors++;
                if (firstActorRecord || !ReferenceEquals(actor.Parent, room.Transform))
                    changed |= SetParent(actor, room.Transform);
                if (firstActorRecord || !actor.Position.Equals(record.ActorPosition) ||
                    !actor.Rotation.Equals(record.ActorRotation))
                    changed |= SetPose(actor, record.ActorPosition, record.ActorRotation);

                if (!Shapes.TryGetValue(shapeKey, out var shape) || shape.Node == null ||
                    shape.Filter == null || shape.Renderer == null)
                {
                    if (shape != null) DestroyShape(shape);
                    Shapes[shapeKey] = shape = CreateOverlayNode<Shape>(
                        $"Shape {record.ShapeId} [{record.ShapeGeneration}]", actor.Transform);
                    shape.Filter = shape.Node.AddComponent<MeshFilter>();
                    shape.Renderer = CreateRenderer(shape.Node);
                    membersChanged = true;
                }
                if (MarkSeen(shape)) seenShapes++;
                changed |= SetParent(shape, actor.Transform);
                changed |= SetPose(shape, record.LocalPosition, record.LocalRotation);
                if (!record.Geometry.Equals(shape.Surface?.Geometry))
                {
                    SetGeometry(shape, record.Geometry);
                    changed = true;
                }
                var material = ActiveMaterials[record.State];
                if (material == null || !ReferenceEquals(shape.Material, material))
                {
                    shape.State = record.State;
                    ApplyShapeMaterial(shape);
                    changed = true;
                }
            }

            if (seenShapes != Shapes.Count)
            {
                FindRemoved(Shapes, RemovedShapes);
                foreach (var key in RemovedShapes)
                {
                    DestroyShape(Shapes[key]);
                    Shapes.Remove(key);
                }
                membersChanged = true;
            }
            if (seenActors != Actors.Count)
            {
                FindRemoved(Actors, RemovedActors);
                foreach (var key in RemovedActors)
                {
                    DestroyNode(Actors[key].Node);
                    Actors.Remove(key);
                }
                changed = true;
            }
            if (seenRooms != Rooms.Count)
            {
                FindRemoved(Rooms, RemovedRooms);
                foreach (var key in RemovedRooms)
                {
                    DestroyNode(Rooms[key].Node);
                    Rooms.Remove(key);
                }
                changed = true;
            }
            if (membersChanged) SetVisibilityMembers();
            lostNode = false;
            if (changed || membersChanged) RepaintViews();
        }

        internal static bool ShowRays(RayRecord[] records) => ShowLines(records, ref rays, parent => new RayBatch(parent));

        internal static bool ShowQueries(QueryLine[] lines) => ShowLines(lines, ref queries, parent => new QueryBatch(parent));

        private static bool ShowLines<T>(T[] records, ref LineBatch<T> batch, System.Func<Transform, LineBatch<T>> create)
        {
            if (records == null)
            {
                if (batch == null) return false;
                DestroyBatch(ref batch);
                if (visibilityGroup != null) SetVisibilityMembers();
                RepaintViews();
                return true;
            }
            if (records.Length == 0)
            {
                if (batch?.Hide() != true) return false;
                RepaintViews();
                return true;
            }

            bool membersChanged = EnsureRoot();
            if (batch == null || !batch.Exists)
            {
                DestroyBatch(ref batch);
                batch = create(Root.transform);
                membersChanged = true;
            }
            bool reparented = batch.Node.transform.parent != Root.transform;
            if (reparented)
                batch.Node.transform.SetParent(Root.transform, false);
            bool changed = batch.Show(records) || membersChanged || reparented;
            if (membersChanged) SetVisibilityMembers();
            if (changed) RepaintViews();
            return changed;
        }

        private static void DestroyBatch<T>(ref LineBatch<T> batch)
        {
            batch?.Destroy();
            batch = null;
        }

        private static bool MarkSeen(OverlayNode node)
        {
            if (node.Seen == frameNumber) return false;
            node.Seen = frameNumber;
            return true;
        }

        private static void FindRemoved<TKey, TNode>(Dictionary<TKey, TNode> nodes, List<TKey> removed)
            where TNode : OverlayNode
        {
            removed.Clear();
            foreach (var entry in nodes)
                if (entry.Value.Seen != frameNumber) removed.Add(entry.Key);
        }

        private static T CreateOverlayNode<T>(string label, Transform parent) where T : OverlayNode, new()
        {
            var node = CreateNode(label, parent);
            return new T { Node = node, Transform = node.transform, Parent = parent };
        }

        private static bool SetParent(OverlayNode node, Transform parent)
        {
            node.Parent = parent;
            if (node.Transform.parent == parent) return false;
            node.Transform.SetParent(parent, false);
            return true;
        }

        private static bool SetPose(OverlayNode node, Vector3 position, Quaternion rotation)
        {
            var transform = node.Transform;
            if (!transform.hasChanged && node.Position.Equals(position) && node.Rotation.Equals(rotation))
                return false;
            node.Position = position;
            node.Rotation = rotation;
            bool changed = !transform.localPosition.Equals(position) || !transform.localRotation.Equals(rotation);
            if (changed) transform.SetLocalPositionAndRotation(position, rotation);
            transform.hasChanged = false;
            return changed;
        }

        private static GameObject CreateNode(string name, Transform parent, HideFlags flags = NodeFlags)
        {
            var node = EditorUtility.CreateGameObjectWithHideFlags(name, flags);
            node.tag = "EditorOnly";
            if (parent != null) node.transform.SetParent(parent, false);
            return node;
        }

        private static MeshRenderer CreateRenderer(GameObject node)
        {
            var renderer = node.AddComponent<MeshRenderer>();
            renderer.shadowCastingMode = ShadowCastingMode.Off;
            renderer.receiveShadows = false;
            renderer.lightProbeUsage = LightProbeUsage.Off;
            renderer.reflectionProbeUsage = ReflectionProbeUsage.Off;
            renderer.motionVectorGenerationMode = MotionVectorGenerationMode.ForceNoMotion;
            return renderer;
        }

        private static void SetGeometry(Shape shape, SurfaceGeometry geometry)
        {
            if (!Meshes.TryGetValue(geometry, out var shared))
            {
                shared = new SharedMesh { Geometry = geometry };
                Meshes.Add(geometry, shared);
            }
            shape.Filter.sharedMesh = shared.GetMesh(wireframe);
            shared.Users++;
            ReleaseSurface(shape.Surface);
            shape.Surface = shared;
        }

        private static void ReleaseSurface(SharedMesh shared)
        {
            if (shared == null || --shared.Users != 0) return;
            Meshes.Remove(shared.Geometry);
            shared.DestroyMeshes();
        }

        private static void ApplyShapeMaterial(Shape shape)
        {
            shape.Material = StateMaterial(shape.State);
            shape.Renderer.sharedMaterial = shape.Material;
            shape.Renderer.enabled = wireframe || surfaceOpacity > 0f;
        }

        private static Material StateMaterial(byte state)
        {
            var cache = ActiveMaterials;
            if (cache[state] != null) return cache[state];
            var pipeline = GraphicsSettings.currentRenderPipeline;
            Material material;
            if (wireframe) material = CreateWireMaterial(pipeline);
            else if (surfaceOpacity < 1f) material = new Material(Shader.Find("Hidden/PVD Studio/Overlay Surface"));
            else if (pipeline != null && pipeline.defaultMaterial != null) material = new Material(pipeline.defaultMaterial);
            else material = new Material(Shader.Find("Standard"));
            material.name = "PVD " + new[] { "Static", "Awake", "Sleeping" }[state];
            material.hideFlags = HideFlags.HideAndDontSave;
            material.color = wireframe ? Colors[state] : SurfaceTint(state);
            material.enableInstancing = true;
            return cache[state] = material;
        }

        private static Material CreateWireMaterial(RenderPipelineAsset pipeline)
        {
            var overlay = Shader.Find("Hidden/PVD Studio/Overlay Wire");
            if (SystemInfo.supportsGeometryShaders && overlay != null && overlay.isSupported)
                return new Material(overlay);
            if (pipeline != null)
            {
                Shader shader = null;
                switch (pipeline.renderPipelineShaderTag)
                {
                    case "UniversalRenderPipeline":
                    case "UniversalPipeline": shader = Shader.Find("Universal Render Pipeline/Unlit"); break;
                    case "HDRenderPipeline": shader = Shader.Find("HDRP/Unlit"); break;
                }
                if (shader != null && shader.isSupported) return new Material(shader);
                var template = pipeline.defaultLineMaterial;
                if (template == null) template = pipeline.defaultMaterial;
                if (template != null) return new Material(template);
            }
            return new Material(Shader.Find("Unlit/Color"));
        }

        private static void DestroyNode(GameObject node)
        {
            if (node == null) return;
            // Owned leaves are removed first. Preserve anything parented here by the user.
            while (node.transform.childCount > 0)
                node.transform.GetChild(node.transform.childCount - 1).SetParent(null, true);
            Object.DestroyImmediate(node);
        }

        private static void DestroyShape(Shape shape)
        {
            DestroyNode(shape.Node);
            ReleaseSurface(shape.Surface);
        }

        private static void Subscribe()
        {
            Camera.onPreCull += BeginCamera;
            Camera.onPostRender += EndCamera;
            RenderPipelineManager.beginCameraRendering += BeginSrpCamera;
            RenderPipelineManager.endCameraRendering += EndSrpCamera;
            RenderPipelineManager.activeRenderPipelineTypeChanged += RefreshMaterials;
            AssemblyReloadEvents.beforeAssemblyReload += Clear;
            EditorApplication.quitting += Clear;
            EditorApplication.hierarchyChanged += CheckHierarchy;
            subscribed = true;
        }

        private static void RefreshMaterials()
        {
            ClearMaterials();
            ApplyShapeAppearance(false);
            if (rays != null && rays.Exists) rays.RefreshMaterials();
            if (queries != null && queries.Exists) queries.RefreshMaterials();
            RepaintViews();
        }

        private static void CheckHierarchy()
        {
            if (visibilityGroup == null || Root == null || Root.transform.parent != null ||
                (rays != null && (!rays.Exists || rays.Node.transform.parent != Root.transform)) ||
                (queries != null && (!queries.Exists || queries.Node.transform.parent != Root.transform)))
            {
                lostNode = true;
                return;
            }
            foreach (var room in Rooms.Values)
                if (room.Node == null || room.Transform.parent != room.Parent) { lostNode = true; return; }
            foreach (var actor in Actors.Values)
                if (actor.Node == null || actor.Transform.parent != actor.Parent) { lostNode = true; return; }
            foreach (var shape in Shapes.Values)
                if (shape.Node == null || shape.Filter == null || shape.Renderer == null ||
                    shape.Transform.parent != shape.Parent)
                {
                    lostNode = true;
                    return;
                }
        }

        private static void BeginSrpCamera(ScriptableRenderContext context, Camera camera) => BeginCamera(camera);
        private static void EndSrpCamera(ScriptableRenderContext context, Camera camera) => EndCamera(camera);

        private static void BeginCamera(Camera camera)
        {
            CameraVisibility.Push(visible);
            var type = camera.cameraType;
            SetVisible(type == CameraType.SceneView || (showInGameView && type == CameraType.Game));
        }

        private static void EndCamera(Camera camera)
        {
            SetVisible(CameraVisibility.Count != 0 && CameraVisibility.Pop());
        }

        private static void SetVisible(bool value)
        {
            if (visible == value) return;
            visible = value;
            if (visibilityGroup != null) visibilityGroup.ForceLOD(value ? 0 : 1);
        }

        private static void SetVisibilityMembers()
        {
            // Force one level per camera. Only owned renderers participate, including when users
            // attach other objects below the overlay. Neither level depends on screen size.
            var rayRenderer = rays?.Renderer;
            var queryRenderer = queries?.Renderer;
            var members = new Renderer[Shapes.Count + (rayRenderer == null ? 0 : 1) + (queryRenderer == null ? 0 : 1)];
            int index = 0;
            foreach (var shape in Shapes.Values) members[index++] = shape.Renderer;
            if (rayRenderer != null) members[index++] = rayRenderer;
            if (queryRenderer != null) members[index] = queryRenderer;
            visibilityGroup.SetLODs(new[] { new LOD(0.5f, members), new LOD(0f, System.Array.Empty<Renderer>()) });
            visibilityGroup.ForceLOD(visible ? 0 : 1);
        }

        internal static void Clear()
        {
            if (subscribed)
            {
                Camera.onPreCull -= BeginCamera;
                Camera.onPostRender -= EndCamera;
                RenderPipelineManager.beginCameraRendering -= BeginSrpCamera;
                RenderPipelineManager.endCameraRendering -= EndSrpCamera;
                RenderPipelineManager.activeRenderPipelineTypeChanged -= RefreshMaterials;
                AssemblyReloadEvents.beforeAssemblyReload -= Clear;
                EditorApplication.quitting -= Clear;
                EditorApplication.hierarchyChanged -= CheckHierarchy;
                subscribed = false;
            }
            CameraVisibility.Clear();
            visible = false;
            lostNode = false;
            DestroyBatch(ref queries);
            DestroyBatch(ref rays);
            foreach (var shape in Shapes.Values) DestroyShape(shape);
            Shapes.Clear();
            foreach (var orphan in Meshes.Values) orphan.DestroyMeshes();
            Meshes.Clear();
            foreach (var actor in Actors.Values) DestroyNode(actor.Node);
            Actors.Clear();
            foreach (var room in Rooms.Values) DestroyNode(room.Node);
            Rooms.Clear();
            DestroyNode(Root);
            Root = null;
            visibilityGroup = null;
            ClearMaterials();
            RepaintViews();
        }

        private static void ClearMaterials()
        {
            for (int i = 0; i < Materials.Length; i++)
            {
                if (Materials[i] != null) Object.DestroyImmediate(Materials[i]);
                Materials[i] = null;
                if (OpaqueMaterials[i] != null) Object.DestroyImmediate(OpaqueMaterials[i]);
                OpaqueMaterials[i] = null;
                if (WireMaterials[i] != null) Object.DestroyImmediate(WireMaterials[i]);
                WireMaterials[i] = null;
            }
        }
    }
}
