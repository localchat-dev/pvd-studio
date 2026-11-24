using System;
using System.Collections.Generic;
using System.IO;
using NUnit.Framework;
using UnityEngine;

namespace PvdStudio.Editor.Tests
{
    public class SurfaceFrameTests
    {
        private static byte[] Frame(byte kind, Action<BinaryWriter> geometry, ulong actorId = 12, float actorX = 1f)
        {
            using (var buffer = new MemoryStream())
            using (var writer = new BinaryWriter(buffer))
            {
                writer.Write(0x31535650u);
                writer.Write(1u);
                writer.Write(3ul);
                writer.Write(actorId);
                writer.Write(4u);
                writer.Write(25ul);
                writer.Write(6u);
                writer.Write((byte)2);
                writer.Write(kind);
                foreach (float value in new float[] { actorX,2,3,0,0,0,1, 4,5,6,0,0,0,1 })
                    writer.Write(value);
                geometry(writer);
                return buffer.ToArray();
            }
        }

        [TestCase(0, 2f, 4f, 6f)]
        [TestCase(1, 2f, 2f, 2f)]
        [TestCase(2, 6f, 2f, 2f)]
        [TestCase(3, 0f, 10f, 10f)]
        public void BuildsFacesInShapeLocalSpace(byte kind, float x, float y, float z)
        {
            var record = SurfaceFrame.Decode(Frame(kind, writer =>
            {
                if (kind < 3) writer.Write(1f);
                if (kind == 0 || kind == 2) writer.Write(2f);
                if (kind == 0) writer.Write(3f);
            }))[0];
            Assert.AreEqual(3ul, record.RoomNumber);
            Assert.AreEqual(12ul, record.ActorId);
            Assert.AreEqual(25ul, record.ShapeId);
            Assert.AreEqual(4u, record.ActorGeneration);
            Assert.AreEqual(6u, record.ShapeGeneration);
            Assert.AreEqual(2, (int)record.State);
            Assert.AreEqual(new Vector3(1, 2, 3), record.ActorPosition);
            Assert.AreEqual(new Vector3(4, 5, 6), record.LocalPosition);
            Mesh mesh = record.Geometry.BuildMesh();
            try
            {
                Assert.AreEqual(new Vector3(x, y, z), mesh.bounds.size);
                var vertices = mesh.vertices;
                var normals = mesh.normals;
                var triangles = mesh.triangles;
                Assert.Greater(triangles.Length, 0);
                for (int i = 0; i < triangles.Length; i += 3)
                {
                    int a = triangles[i], b = triangles[i + 1], c = triangles[i + 2];
                    var face = Vector3.Cross(vertices[b] - vertices[a], vertices[c] - vertices[a]);
                    Assert.Greater(Vector3.Dot(face, normals[a]), 0f);
                    if (kind < 3) Assert.Greater(Vector3.Dot(face, vertices[a]), 0f);
                }
            }
            finally { UnityEngine.Object.DestroyImmediate(mesh); }
        }

        private static byte[] Triangle(float y = 1f, uint lastIndex = 2)
        {
            return Frame(4, writer =>
            {
                writer.Write(3u);
                writer.Write(3u);
                foreach (float v in new float[] { 0,0,0, 1,0,0, 0,y,0 }) writer.Write(v);
                writer.Write(0u);
                writer.Write(1u);
                writer.Write(lastIndex);
            });
        }

        private static HashSet<(int, int)> WireEdges(Mesh mesh)
        {
            Assert.AreEqual(MeshTopology.Lines, mesh.GetTopology(0));
            var indices = mesh.GetIndices(0);
            Assert.AreEqual(0, indices.Length % 2);
            var edges = new HashSet<(int, int)>();
            for (int i = 0; i < indices.Length; i += 2)
            {
                int a = indices[i], b = indices[i + 1];
                Assert.AreNotEqual(a, b);
                Assert.IsTrue(edges.Add((Math.Min(a, b), Math.Max(a, b))));
            }
            return edges;
        }

        [TestCase(0, 12)]
        [TestCase(3, 4)]
        public void BoxAndPlaneWiresFollowOnlyPhysicalEdges(byte kind, int edgeCount)
        {
            var mesh = new SurfaceGeometry(kind, new Vector3(1, 2, 3)).BuildWireMesh();
            try
            {
                var edges = WireEdges(mesh);
                Assert.AreEqual(edgeCount, edges.Count);
                var extents = kind == 0 ? new Vector3(1, 2, 3) : new Vector3(0, 5, 5);
                var vertices = mesh.vertices;
                Assert.AreEqual(kind == 0 ? 8 : 4, new HashSet<Vector3>(vertices).Count);
                foreach (var p in vertices)
                    Assert.AreEqual(extents, new Vector3(Math.Abs(p.x), Math.Abs(p.y), Math.Abs(p.z)));
                foreach (var edge in edges)
                {
                    var a = vertices[edge.Item1];
                    var b = vertices[edge.Item2];
                    int axes = (a.x == b.x ? 0 : 1) + (a.y == b.y ? 0 : 1) + (a.z == b.z ? 0 : 1);
                    Assert.AreEqual(1, axes);
                }
            }
            finally { UnityEngine.Object.DestroyImmediate(mesh); }
        }

        [TestCase(1, 0f)]
        [TestCase(2, 2f)]
        [TestCase(2, 0f)]
        public void RoundedWiresTraceRingsAndMeridians(byte kind, float halfHeight)
        {
            var mesh = new SurfaceGeometry(kind, new Vector3(1, halfHeight, 0)).BuildWireMesh();
            try
            {
                var vertices = mesh.vertices;
                var ringPositions = new HashSet<float>();
                int leftPoleEdges = 0, rightPoleEdges = 0;
                foreach (var edge in WireEdges(mesh))
                {
                    var a = vertices[edge.Item1];
                    var b = vertices[edge.Item2];
                    Assert.AreNotEqual(a, b);
                    if (a.x == b.x)
                        ringPositions.Add(a.x);
                    else
                        Assert.AreEqual(0f, a.y * b.z - a.z * b.y, 0.00001f);
                    if (Math.Min(a.x, b.x) == -halfHeight - 1) leftPoleEdges++;
                    if (Math.Max(a.x, b.x) == halfHeight + 1) rightPoleEdges++;
                }
                Assert.IsTrue(ringPositions.Contains(-halfHeight));
                Assert.IsTrue(ringPositions.Contains(halfHeight));
                Assert.Greater(leftPoleEdges, 2);
                Assert.AreEqual(leftPoleEdges, rightPoleEdges);
                Assert.AreEqual(new Vector3(2 * (halfHeight + 1), 2, 2), mesh.bounds.size);
            }
            finally { UnityEngine.Object.DestroyImmediate(mesh); }
        }

        [Test]
        public void CapturedWireKeepsDistinctIndexedEdgesIncludingHighIndices()
        {
            const int high = 65536;
            var vertices = new Vector3[high + 1];
            var geometry = new SurfaceGeometry(4, Vector3.zero, vertices,
                new[] { 0, 1, 2, 2, 1, high, 1, 1, 2 });
            var mesh = geometry.BuildWireMesh();
            try
            {
                Assert.AreEqual(UnityEngine.Rendering.IndexFormat.UInt32, mesh.indexFormat);
                Assert.IsTrue(WireEdges(mesh).SetEquals(new[] { (0, 1), (0, 2), (1, 2), (1, high), (2, high) }));
            }
            finally { UnityEngine.Object.DestroyImmediate(mesh); }
        }

        [Test]
        public void MeshReuseComparesVerticesAndTriangleIndices()
        {
            var first = SurfaceFrame.Decode(Triangle())[0].Geometry;
            var same = SurfaceFrame.Decode(Triangle())[0].Geometry;
            Assert.IsTrue(first.Equals(same));
            Assert.AreEqual(first.GetHashCode(), same.GetHashCode());
            Assert.IsFalse(first.Equals(SurfaceFrame.Decode(Triangle(2))[0].Geometry));
            Assert.IsFalse(first.Equals(SurfaceFrame.Decode(Triangle(lastIndex: 1))[0].Geometry));
            Mesh mesh = first.BuildMesh();
            try { CollectionAssert.AreEqual(new[] { 0, 1, 2 }, mesh.triangles); }
            finally { UnityEngine.Object.DestroyImmediate(mesh); }
        }

        private static byte[] Cached(ulong token, ulong previous, byte[] full)
        {
            using (var stream = new MemoryStream())
            using (var writer = new BinaryWriter(stream))
            {
                writer.Write(0x32535650u);
                writer.Write(token);
                writer.Write(previous);
                writer.Write(full, 4, full.Length - 4);
                return stream.ToArray();
            }
        }

        [Test]
        public void CachedFramesReuseOnlyAcknowledgedMeshAndCommitAfterValidation()
        {
            var reader = new SurfaceFrameReader();
            var reference = Frame(5, w => {});
            Assert.AreEqual(0ul, reader.Token);
            var original = reader.Decode(Cached(7, 0, Triangle()))[0].Geometry;
            Assert.AreEqual(7ul, reader.Token);
            var repeated = reader.Decode(Cached(8, 7, reference))[0].Geometry;
            Assert.IsTrue(ReferenceEquals(original, repeated));
            Assert.Throws<InvalidDataException>(() => reader.Decode(Cached(9, 7, reference)));
            Assert.Throws<InvalidDataException>(() => reader.Decode(Cached(9, 0, reference)));
            var wrongIdentity = (byte[])reference.Clone();
            wrongIdentity[8 + 16]++; // Actor generation
            Assert.Throws<InvalidDataException>(() => reader.Decode(Cached(9, 8, wrongIdentity)));
            var trailing = Cached(9, 8, reference);
            Array.Resize(ref trailing, trailing.Length + 1);
            Assert.Throws<InvalidDataException>(() => reader.Decode(trailing));
            Assert.AreEqual(8ul, reader.Token);
            var changed = reader.Decode(Cached(9, 8, Triangle(2)))[0].Geometry;
            Assert.IsFalse(original.Equals(changed));
            reader.Decode(Cached(10, 9, Frame(3, w => {})));
            Assert.Throws<InvalidDataException>(() => reader.Decode(Cached(11, 10, reference)));
        }

        private static byte[] Resources(ulong token, ulong previous,
            (ulong id, byte[] mesh)[] definitions, params byte[][] shapes)
        {
            using (var stream = new MemoryStream())
            using (var writer = new BinaryWriter(stream))
            {
                writer.Write(0x33535650u);
                writer.Write(token);
                writer.Write(previous);
                writer.Write((uint)definitions.Length);
                foreach (var definition in definitions)
                {
                    writer.Write(definition.id);
                    const int geometryOffset = 8 + 90;
                    writer.Write(definition.mesh, geometryOffset, definition.mesh.Length - geometryOffset);
                }
                writer.Write((uint)shapes.Length);
                foreach (var shape in shapes) writer.Write(shape, 8, shape.Length - 8);
                return stream.ToArray();
            }
        }

        private static byte[] MeshReference(ulong id, ulong actorId = 12, float actorX = 1f)
            => Frame(4, w => w.Write(id), actorId, actorX);

        private static byte[] ViewFrame(byte[] resources, Action<BinaryWriter> rays)
        {
            using (var stream = new MemoryStream())
            using (var writer = new BinaryWriter(stream))
            {
                writer.Write(0x34535650u);
                writer.Write(resources, 4, resources.Length - 4);
                rays(writer);
                return stream.ToArray();
            }
        }

        [Test]
        public void ViewFramesReuseResourceTokensInBothDirections()
        {
            var reader = new SurfaceFrameReader();
            var noDefinitions = Array.Empty<(ulong, byte[])>();
            var reference = MeshReference(10);
            var original = reader.Decode(Resources(1, 0, new[] { (10ul, Triangle()) }, reference))[0].Geometry;
            var view = ViewFrame(Resources(2, 1, noDefinitions, reference), w => { w.Write(0u); w.Write(0u); });
            Assert.AreSame(original, reader.Decode(view)[0].Geometry);
            Assert.AreSame(original, reader.Decode(Resources(3, 2, noDefinitions, reference))[0].Geometry);
        }

        [Test]
        public void SurfaceOnlyViewReportsOmittedRaysAndRetainsGeometry()
        {
            var reader = new SurfaceFrameReader();
            var reference = MeshReference(10);
            var surfaces = Resources(1, 0, new[] { (10ul, Triangle()) }, reference);
            var viewWithoutRaySection = reader.DecodeSnapshot(surfaces, expectRays: true);
            Assert.IsTrue(viewWithoutRaySection.RaysTruncated);
            Assert.AreEqual(0, viewWithoutRaySection.Rays.Length);

            var ordinaryFrame = reader.DecodeSnapshot(Resources(2, 1, Array.Empty<(ulong, byte[])>(), reference));
            Assert.IsFalse(ordinaryFrame.RaysTruncated);
            Assert.AreSame(viewWithoutRaySection.Records[0].Geometry, ordinaryFrame.Records[0].Geometry);
        }

        private static void WriteRay(BinaryWriter writer, byte flags, Vector3 start, Vector3 end, Vector3 marker)
        {
            writer.Write(flags);
            foreach (var point in new[] { start, end, marker })
            {
                writer.Write(point.x);
                writer.Write(point.y);
                writer.Write(point.z);
            }
        }

        [Test]
        public void ViewFrameExposesHitAndMissRaysWithoutRequiringSurfaces()
        {
            var reader = new SurfaceFrameReader();
            var start = new Vector3(1, 2, 3);
            var end = new Vector3(4, 5, 6);
            var marker = new Vector3(3, 4, 5);
            var noDefinitions = Array.Empty<(ulong, byte[])>();
            var view = ViewFrame(Resources(1, 0, noDefinitions), w =>
            {
                w.Write(1u);
                w.Write(3u);
                WriteRay(w, 3, start, end, marker);
                WriteRay(w, 0, end, start, Vector3.zero);
                WriteRay(w, 1, start, end, Vector3.zero);
            });

            var snapshot = reader.DecodeSnapshot(view);
            Assert.AreEqual(0, snapshot.Records.Length);
            Assert.IsTrue(snapshot.RaysTruncated);
            Assert.AreEqual(3, snapshot.Rays.Length);
            var hit = snapshot.Rays[0];
            Assert.AreEqual(start, hit.Start);
            Assert.AreEqual(end, hit.End);
            Assert.AreEqual(marker, hit.Marker);
            Assert.IsTrue(hit.Hit);
            Assert.IsTrue(hit.HasMarker);
            var miss = snapshot.Rays[1];
            Assert.AreEqual(end, miss.Start);
            Assert.AreEqual(start, miss.End);
            Assert.AreEqual(Vector3.zero, miss.Marker);
            Assert.IsFalse(miss.Hit);
            Assert.IsFalse(miss.HasMarker);
            Assert.IsTrue(snapshot.Rays[2].Hit);
            Assert.IsFalse(snapshot.Rays[2].HasMarker);

            var emptyView = ViewFrame(Resources(2, 1, noDefinitions), w => { w.Write(0u); w.Write(0u); });
            var legacyFrames = new[] { emptyView, Resources(3, 2, noDefinitions), Cached(4, 0, Triangle()), Triangle() };
            foreach (var frame in legacyFrames)
            {
                var empty = reader.DecodeSnapshot(frame);
                Assert.AreEqual(0, empty.Rays.Length);
                Assert.IsFalse(empty.RaysTruncated);
            }
        }

        [Test]
        public void MalformedRaySectionKeepsPreviousTokenAndResources()
        {
            var reader = new SurfaceFrameReader();
            var originalReference = MeshReference(10);
            var original = reader.Decode(Resources(1, 0, new[] { (10ul, Triangle()) }, originalReference))[0].Geometry;
            var replacement = Resources(2, 1, new[] { (20ul, Triangle(2)) }, MeshReference(20));
            var zero = Vector3.zero;
            var nonFinite = new Vector3(0, float.NaN, 0);
            Action<BinaryWriter>[] malformedSections = {
                w => w.Write(0u),
                w => { w.Write(2u); w.Write(0u); },
                w => { w.Write(0u); w.Write(2049u); w.Write(new byte[2049 * 37]); },
                w => { w.Write(0u); w.Write(1u); },
                w => { w.Write(0u); w.Write(0u); w.Write((byte)0); },
                w => { w.Write(0u); w.Write(1u); WriteRay(w, 4, zero, zero, zero); },
                w => { w.Write(0u); w.Write(1u); WriteRay(w, 2, zero, zero, zero); },
                w => { w.Write(0u); w.Write(1u); WriteRay(w, 1, zero, zero, new Vector3(1, 0, 0)); },
                w => { w.Write(0u); w.Write(1u); WriteRay(w, 0, nonFinite, zero, zero); },
                w => { w.Write(0u); w.Write(1u); WriteRay(w, 0, zero, nonFinite, zero); },
                w => { w.Write(0u); w.Write(1u); WriteRay(w, 3, zero, zero, nonFinite); }
            };
            foreach (var malformed in malformedSections)
            {
                Assert.Throws<InvalidDataException>(() => reader.DecodeSnapshot(ViewFrame(replacement, malformed)));
                Assert.AreEqual(1ul, reader.Token);
            }
            var retry = reader.Decode(Resources(2, 1, Array.Empty<(ulong, byte[])>(), originalReference));
            Assert.AreSame(original, retry[0].Geometry);
        }

        private static byte[] QueryFrame(byte[] view, Action<BinaryWriter> queries)
        {
            using (var stream = new MemoryStream())
            using (var writer = new BinaryWriter(stream))
            {
                writer.Write(0x35535650u);
                writer.Write(view, 4, view.Length - 4);
                queries(writer);
                return stream.ToArray();
            }
        }

        private static void WriteQueryLine(BinaryWriter writer, byte style, Vector3 start, Vector3 end)
        {
            writer.Write(style);
            writer.Write(start.x);
            writer.Write(start.y);
            writer.Write(start.z);
            writer.Write(end.x);
            writer.Write(end.y);
            writer.Write(end.z);
        }

        [Test]
        public void QueryFrameSharesMeshResourcesWithSurfaceAndRayFrames()
        {
            var reader = new SurfaceFrameReader();
            var reference = MeshReference(10);
            var noDefinitions = Array.Empty<(ulong, byte[])>();
            var start = new Vector3(1, 2, 3);
            var end = new Vector3(4, 5, 6);
            var original = reader.Decode(Resources(1, 0, new[] { (10ul, Triangle()) }, reference))[0].Geometry;
            var view = ViewFrame(Resources(2, 1, noDefinitions, reference), w =>
            {
                w.Write(0u);
                w.Write(1u);
                WriteRay(w, 0, start, end, Vector3.zero);
            });
            var query = QueryFrame(view, w =>
            {
                w.Write(1u);
                w.Write(3u);
                WriteQueryLine(w, 0, start, end);
                WriteQueryLine(w, 1, end, start);
                WriteQueryLine(w, 2, end, Vector3.zero);
            });

            var snapshot = reader.DecodeSnapshot(query);
            Assert.AreSame(original, snapshot.Records[0].Geometry);
            Assert.AreEqual(1, snapshot.Rays.Length);
            Assert.AreEqual(end, snapshot.Rays[0].End);
            Assert.IsFalse(snapshot.RaysTruncated);
            Assert.IsTrue(snapshot.QueriesTruncated);
            Assert.AreEqual(3, snapshot.Queries.Length);
            Assert.AreEqual((byte)0, snapshot.Queries[0].Style);
            Assert.AreEqual(start, snapshot.Queries[0].Start);
            Assert.AreEqual(end, snapshot.Queries[0].End);
            Assert.AreEqual((byte)1, snapshot.Queries[1].Style);
            Assert.AreEqual(end, snapshot.Queries[1].Start);
            Assert.AreEqual(start, snapshot.Queries[1].End);
            Assert.AreEqual((byte)2, snapshot.Queries[2].Style);
            Assert.AreEqual(end, snapshot.Queries[2].Start);
            Assert.AreEqual(Vector3.zero, snapshot.Queries[2].End);
            var rays = ViewFrame(Resources(3, 2, noDefinitions, reference), w => { w.Write(0u); w.Write(0u); });
            Assert.AreSame(original, reader.Decode(rays)[0].Geometry);
            Assert.AreSame(original, reader.Decode(Resources(4, 3, noDefinitions, reference))[0].Geometry);
        }

        [Test]
        public void EmptyQueryFrameAndOverlayBudgetFallbackHaveDistinctFlags()
        {
            var reader = new SurfaceFrameReader();
            var noDefinitions = Array.Empty<(ulong, byte[])>();
            var emptyView = ViewFrame(Resources(1, 0, noDefinitions), w => { w.Write(0u); w.Write(0u); });
            var snapshot = reader.DecodeSnapshot(QueryFrame(emptyView, w => { w.Write(0u); w.Write(0u); }),
                expectRays: true, expectQueries: true);
            Assert.AreEqual(0, snapshot.Rays.Length);
            Assert.AreEqual(0, snapshot.Queries.Length);
            Assert.IsFalse(snapshot.RaysTruncated);
            Assert.IsFalse(snapshot.QueriesTruncated);

            var budgetFallback = reader.DecodeSnapshot(Resources(2, 1, noDefinitions),
                expectRays: true, expectQueries: true);
            Assert.AreEqual(0, budgetFallback.Rays.Length);
            Assert.AreEqual(0, budgetFallback.Queries.Length);
            Assert.IsTrue(budgetFallback.RaysTruncated);
            Assert.IsTrue(budgetFallback.QueriesTruncated);

            var ordinaryFrame = reader.DecodeSnapshot(Resources(3, 2, noDefinitions));
            Assert.IsFalse(ordinaryFrame.RaysTruncated);
            Assert.IsFalse(ordinaryFrame.QueriesTruncated);
        }

        [Test]
        public void MalformedQuerySectionKeepsPreviousTokenAndResources()
        {
            var reader = new SurfaceFrameReader();
            var reference = MeshReference(10);
            var original = reader.Decode(Resources(1, 0, new[] { (10ul, Triangle()) }, reference))[0].Geometry;
            var view = ViewFrame(Resources(2, 1, new[] { (20ul, Triangle(2)) }, MeshReference(20)),
                w => { w.Write(0u); w.Write(0u); });
            var zero = Vector3.zero;
            var nonFinite = new Vector3(0, float.PositiveInfinity, 0);
            Action<BinaryWriter>[] malformedSections = {
                w => w.Write(0u),
                w => { w.Write(2u); w.Write(0u); },
                w => { w.Write(0u); w.Write(8193u); w.Write(new byte[8193 * 25]); },
                w => { w.Write(0u); w.Write(1u); },
                w => { w.Write(0u); w.Write(0u); w.Write((byte)0); },
                w => { w.Write(0u); w.Write(1u); WriteQueryLine(w, 3, zero, zero); },
                w => { w.Write(0u); w.Write(1u); WriteQueryLine(w, 0, nonFinite, zero); },
                w => { w.Write(0u); w.Write(1u); WriteQueryLine(w, 0, zero, nonFinite); }
            };
            foreach (var malformed in malformedSections)
            {
                Assert.Throws<InvalidDataException>(() => reader.DecodeSnapshot(QueryFrame(view, malformed)));
                Assert.AreEqual(1ul, reader.Token);
            }
            var retry = reader.Decode(Resources(2, 1, Array.Empty<(ulong, byte[])>(), reference));
            Assert.AreSame(original, retry[0].Geometry);
        }

        [Test]
        public void ResourceFramesShareGeometryAcrossActorsAndDropRemovedResources()
        {
            var reader = new SurfaceFrameReader();
            var noDefinitions = Array.Empty<(ulong, byte[])>();
            var first = MeshReference(10);
            var second = MeshReference(10, actorId: 13, actorX: 9f);
            var records = reader.Decode(Resources(1, 0, new[] { (10ul, Triangle()) },
                first, second, Frame(1, w => w.Write(2f))));
            Assert.AreEqual(3, records.Length);
            Assert.AreEqual(13ul, records[1].ActorId);
            Assert.AreEqual(9f, records[1].ActorPosition.x);
            Assert.AreSame(records[0].Geometry, records[1].Geometry);
            Assert.AreEqual(1, (int)records[2].Geometry.Kind);
            var motion = reader.Decode(Resources(2, 1, noDefinitions, second));
            Assert.AreSame(records[0].Geometry, motion[0].Geometry);
            var changed = reader.Decode(Resources(3, 2, new[] { (11ul, Triangle(2)) }, MeshReference(11)));
            Assert.IsFalse(changed[0].Geometry.Equals(records[0].Geometry));
            Assert.Throws<InvalidDataException>(() => reader.Decode(Resources(4, 3, noDefinitions, first)));
            Assert.Throws<InvalidDataException>(() => reader.Decode(Resources(4, 0, noDefinitions, MeshReference(11))));
            var reset = reader.Decode(Resources(4, 0, new[] { (11ul, Triangle()) }, MeshReference(11)));
            Assert.IsFalse(reset[0].Geometry.Equals(changed[0].Geometry));
            Assert.IsTrue(reset[0].Geometry.Equals(records[0].Geometry));
        }

        [Test]
        public void InvalidResourceFramesPreserveTheAcknowledgedDictionary()
        {
            var reader = new SurfaceFrameReader();
            var noDefinitions = Array.Empty<(ulong, byte[])>();
            var reference = MeshReference(10);
            var original = reader.Decode(Resources(7, 0, new[] { (10ul, Triangle()) }, reference))[0].Geometry;
            var badRotation = MeshReference(20);
            Array.Clear(badRotation, 8 + 34 + 12, 16);
            var trailing = Resources(8, 7, noDefinitions, reference);
            Array.Resize(ref trailing, trailing.Length + 1);
            foreach (var frame in new[] {
                Resources(8, 7, new[] { (10ul, Triangle(2)) }, reference),
                Resources(8, 7, new[] { (20ul, Triangle()), (20ul, Triangle()) },
                    MeshReference(20), MeshReference(20, actorId: 13)),
                Resources(8, 7, new[] { (20ul, Triangle()) }, reference),
                Resources(8, 7, new[] { (0ul, Triangle()) }, MeshReference(0)),
                Resources(8, 7, new[] { (20ul, Triangle(lastIndex: 3)) }, MeshReference(20)),
                Resources(8, 7, new[] { (20ul, Triangle()) }, badRotation),
                Resources(8, 7, noDefinitions, MeshReference(20)),
                Resources(8, 7, noDefinitions, Frame(5, w => {})),
                Resources(7, 7, noDefinitions), Resources(0, 7, noDefinitions),
                Resources(8, 6, noDefinitions), trailing,
                Cached(8, 7, Triangle()) })
            {
                Assert.Throws<InvalidDataException>(() => reader.Decode(frame));
                Assert.AreEqual(7ul, reader.Token);
            }
            Assert.AreSame(original, reader.Decode(Resources(8, 7, noDefinitions, reference))[0].Geometry);
            reader.Decode(Cached(9, 0, Triangle()));
            Assert.Throws<InvalidDataException>(() => reader.Decode(Resources(10, 9, noDefinitions)));
        }

        [Test]
        public void SharedMeshPayloadIsChargedOnceForThousandsOfInstances()
        {
            const int indexCount = 18000;
            var mesh = Frame(4, w =>
            {
                w.Write(1u);
                w.Write((uint)indexCount);
                w.Write(new byte[12 + indexCount * 4]);
            });
            var shapes = new byte[2000][];
            for (int i = 0; i < shapes.Length; i++) shapes[i] = MeshReference(1, actorId: (ulong)i + 1);
            var records = SurfaceFrame.Decode(Resources(1, 0, new[] { (1ul, mesh) }, shapes));
            Assert.AreEqual(shapes.Length, records.Length);
            var shared = records[0].Geometry;
            Assert.IsTrue(shared.MeshPayloadBytes * records.Length > 128L * 1024 * 1024);
            foreach (var record in records) Assert.AreSame(shared, record.Geometry);
        }

        // One vertex and repeated degenerate triangles exercise byte budgets without a second large buffer.
        private static byte[] BudgetFrame(int number, int triangles)
        {
            int indexBytes = triangles * 12;
            int shapeCount = number + 1;
            var bytes = new byte[24 + 8 + 20 + indexBytes + 4 + shapeCount * 98];
            using (var stream = new MemoryStream(bytes))
            using (var writer = new BinaryWriter(stream))
            {
                writer.Write(0x33535650u);
                writer.Write((ulong)number);
                writer.Write((ulong)(number - 1));
                writer.Write(1u);
                writer.Write((ulong)number);
                writer.Write(1u);
                writer.Write((uint)(triangles * 3));
                writer.Write((float)number);
                stream.Position += 8 + indexBytes;
                writer.Write((uint)shapeCount);
                for (int id = 1; id <= shapeCount; id++)
                {
                    var shape = MeshReference((ulong)Math.Min(id, number), actorId: (ulong)id);
                    writer.Write(shape, 8, shape.Length - 8);
                }
            }
            return bytes;
        }

        [Test, Explicit("Exercises the production 32 MiB packet and 128 MiB mesh resource limits.")]
        public void ResourceLimitsCountWireBytesAndRetainedUniqueGeometry()
        {
            const int mib = 1024 * 1024;
            var reader = new SurfaceFrameReader();
            Assert.Throws<InvalidDataException>(() => reader.Decode(BudgetFrame(1, 32 * mib / 12)));
            const int triangles = (20 * mib - 20) / 12;
            for (int number = 1; number <= 6; number++) reader.Decode(BudgetFrame(number, triangles));
            int remaining = 128 * mib - 6 * (20 + triangles * 12);
            var records = reader.Decode(BudgetFrame(7, (remaining - 20) / 12));
            Assert.AreSame(records[6].Geometry, records[7].Geometry);
            Assert.Throws<InvalidDataException>(() => reader.Decode(BudgetFrame(8, 1)));
            Assert.AreEqual(7ul, reader.Token);
            var kept = reader.Decode(Resources(8, 7, Array.Empty<(ulong, byte[])>(), MeshReference(7)));
            Assert.AreSame(records[6].Geometry, kept[0].Geometry);
        }

        [Test, Explicit("Measure managed decoder time and allocation on this runtime.")]
        public void MeasureRepeatedGridDecode()
        {
            const int side = 64, samples = 64;
            var full = Frame(4, writer =>
            {
                writer.Write((uint)((side + 1) * (side + 1)));
                writer.Write((uint)(side * side * 6));
                for (int y = 0; y <= side; y++)
                    for (int x = 0; x <= side; x++)
                    {
                        writer.Write((float)x);
                        writer.Write((float)y);
                        writer.Write(0f);
                    }
                for (int y = 0; y < side; y++)
                    for (int x = 0; x < side; x++)
                    {
                        int a = y * (side + 1) + x, b = a + side + 1;
                        foreach (int index in new[] { a, a + 1, b, b, a + 1, b + 1 }) writer.Write(index);
                    }
            });
            var reader = new SurfaceFrameReader();
            reader.Decode(Cached(1, 0, full));
            var reference = Frame(5, writer => {});
            var deltas = new byte[samples + 1][];
            for (int i = 0; i < deltas.Length; i++)
                deltas[i] = Cached((ulong)i + 2, (ulong)i + 1, reference);
            Measure("PVS1 full grid", samples, i => SurfaceFrame.Decode(full));
            Measure("PVS2 reused grid", samples, i => reader.Decode(deltas[i]));
        }

        private static void Measure(string name, int samples, Func<int, SurfaceRecord[]> decode)
        {
            var elapsed = new long[samples];
            decode(0);
            long startBytes = GC.GetAllocatedBytesForCurrentThread();
            for (int i = 0; i < samples; i++)
            {
                long start = System.Diagnostics.Stopwatch.GetTimestamp();
                var records = decode(i + 1);
                elapsed[i] = System.Diagnostics.Stopwatch.GetTimestamp() - start;
                GC.KeepAlive(records);
            }
            long bytes = GC.GetAllocatedBytesForCurrentThread() - startBytes;
            Array.Sort(elapsed);
            double medianMs = (elapsed[samples / 2 - 1] + elapsed[samples / 2])
                * 500.0 / System.Diagnostics.Stopwatch.Frequency;
            Console.WriteLine("{0}: {1:F4} ms median, {2} allocated bytes/frame ({3} samples)",
                name, medianMs, bytes / samples, samples);
        }

        [Test]
        public void RejectsIncompleteUnboundedAndInvalidRecords()
        {
            var truncated = Triangle();
            Array.Resize(ref truncated, truncated.Length - 1);
            var trailing = Triangle();
            Array.Resize(ref trailing, trailing.Length + 1);
            var oversized = Triangle();
            oversized[7] = 0xff;
            var invalidPose = Triangle();
            Array.Clear(invalidPose, 8 + 34 + 12, 16);
            foreach (var frame in new[] { truncated, trailing, oversized, invalidPose, Triangle(float.NaN),
                Triangle(lastIndex: 3), Frame(4, w => { w.Write(uint.MaxValue); w.Write(3u); }),
                Frame(1, w => w.Write(-1f)), Frame(255, w => {}) })
                Assert.Throws<InvalidDataException>(() => SurfaceFrame.Decode(frame));
        }
    }
}
