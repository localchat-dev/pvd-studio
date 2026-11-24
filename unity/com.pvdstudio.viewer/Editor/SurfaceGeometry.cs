using System;
using System.Collections.Generic;
using UnityEngine;
using UnityEngine.Rendering;

namespace PvdStudio.Editor
{
    internal sealed class SurfaceGeometry : IEquatable<SurfaceGeometry>
    {
        internal byte Kind { get; }
        private readonly Vector3 size;
        private readonly Vector3[] vertices;
        private readonly int[] indices;
        private readonly int hash;
        internal long MeshPayloadBytes => 8L + vertices.Length * 12L + indices.Length * 4L;

        internal SurfaceGeometry(byte kind, Vector3 size, Vector3[] vertices = null, int[] indices = null)
        {
            Kind = kind;
            this.size = size;
            this.vertices = vertices;
            this.indices = indices;
            unchecked
            {
                hash = kind * 31 + size.GetHashCode();
                if (vertices != null)
                    foreach (var vertex in vertices) hash = hash * 31 + vertex.GetHashCode();
                if (indices != null)
                    foreach (var index in indices) hash = hash * 31 + index;
            }
        }

        public bool Equals(SurfaceGeometry other)
        {
            if (ReferenceEquals(this, other)) return true;
            if (other == null || Kind != other.Kind || hash != other.hash || !size.Equals(other.size))
                return false;
            if (Kind != 4) return true;
            if (vertices.Length != other.vertices.Length || indices.Length != other.indices.Length) return false;
            for (int i = 0; i < vertices.Length; i++)
                if (!vertices[i].Equals(other.vertices[i])) return false;
            for (int i = 0; i < indices.Length; i++)
                if (indices[i] != other.indices[i]) return false;
            return true;
        }

        public override bool Equals(object other) => Equals(other as SurfaceGeometry);
        public override int GetHashCode() => hash;

        internal Mesh BuildMesh() => BuildMesh(MeshTopology.Triangles);
        internal Mesh BuildWireMesh() => BuildMesh(MeshTopology.Lines);

        private Mesh BuildMesh(MeshTopology topology)
        {
            Vector3[] points = vertices;
            int[] elements = indices;
            bool wire = topology == MeshTopology.Lines;
            switch (Kind)
            {
                case 0: Box(wire, out points, out elements); break;
                case 1: Rounded(size.x, 0f, wire, out points, out elements); break;
                case 2: Rounded(size.x, size.y, wire, out points, out elements); break;
                case 3: Plane(wire, out points, out elements); break;
                case 4:
                    if (wire) elements = TriangleEdges(indices);
                    break;
            }
            var mesh = new Mesh();
            try
            {
                mesh.name = wire ? "PVD wireframe" : "PVD surface";
                mesh.hideFlags = HideFlags.HideAndDontSave;
                if (points.Length > ushort.MaxValue) mesh.indexFormat = IndexFormat.UInt32;
                mesh.vertices = points;
                if (wire) mesh.SetIndices(elements, MeshTopology.Lines, 0);
                else
                {
                    mesh.triangles = elements;
                    mesh.RecalculateNormals();
                    mesh.RecalculateBounds();
                }
                return mesh;
            }
            catch
            {
                UnityEngine.Object.DestroyImmediate(mesh);
                throw;
            }
        }

        private static int[] TriangleEdges(int[] triangles)
        {
            var seen = new HashSet<(int, int)>();
            var lines = new List<int>();
            void AddEdge(int a, int b)
            {
                if (a == b || !seen.Add((Math.Min(a, b), Math.Max(a, b)))) return;
                lines.Add(a);
                lines.Add(b);
            }
            for (int i = 0; i < triangles.Length; i += 3)
            {
                int a = triangles[i], b = triangles[i + 1], c = triangles[i + 2];
                AddEdge(a, b);
                AddEdge(b, c);
                AddEdge(c, a);
            }
            return lines.ToArray();
        }

        private static void Triangle(List<int> faces, int a, int b, int c)
        {
            faces.Add(a);
            faces.Add(b);
            faces.Add(c);
        }

        // Each quad owns its vertices so box edges and the two plane faces keep distinct normals.
        private static void Quad(List<Vector3> points, List<int> faces, Vector3 a, Vector3 b, Vector3 c, Vector3 d)
        {
            int start = points.Count;
            points.Add(a);
            points.Add(b);
            points.Add(c);
            points.Add(d);
            Triangle(faces, start, start + 1, start + 2);
            Triangle(faces, start, start + 2, start + 3);
        }

        private void Box(bool wire, out Vector3[] points, out int[] indices)
        {
            var corners = new Vector3[8];
            for (int i = 0; i < corners.Length; i++)
                corners[i] = new Vector3((i & 1) == 0 ? -size.x : size.x,
                    (i & 2) == 0 ? -size.y : size.y, (i & 4) == 0 ? -size.z : size.z);
            if (wire)
            {
                points = corners;
                indices = new[] { 0,1, 2,3, 4,5, 6,7, 0,2, 1,3, 4,6, 5,7, 0,4, 1,5, 2,6, 3,7 };
                return;
            }
            var p = new List<Vector3>(24);
            var f = new List<int>(36);
            int[] order = { 0,4,6,2, 1,3,7,5, 0,1,5,4, 2,6,7,3, 0,2,3,1, 4,5,7,6 };
            for (int i = 0; i < order.Length; i += 4)
                Quad(p, f, corners[order[i]], corners[order[i + 1]], corners[order[i + 2]], corners[order[i + 3]]);
            points = p.ToArray();
            indices = f.ToArray();
        }

        private static void Plane(bool wire, out Vector3[] points, out int[] indices)
        {
            var a = new Vector3(0, -5, -5);
            var b = new Vector3(0, 5, -5);
            var c = new Vector3(0, 5, 5);
            var d = new Vector3(0, -5, 5);
            if (wire)
            {
                points = new[] { a, b, c, d };
                indices = new[] { 0,1, 1,2, 2,3, 3,0 };
                return;
            }
            var p = new List<Vector3>(8);
            var f = new List<int>(12);
            Quad(p, f, a, b, c, d);
            Quad(p, f, d, c, b, a);
            points = p.ToArray();
            indices = f.ToArray();
        }

        private const int Segments = 24;
        private const int HemisphereRings = 6;

        private static void Rounded(float radius, float halfHeight, bool wire, out Vector3[] points, out int[] indices)
        {
            var p = new List<Vector3> { new Vector3(-halfHeight - radius, 0, 0) };
            for (int side = -1; side <= 1; side += 2)
            {
                for (int ring = 0; ring < HemisphereRings; ring++)
                {
                    int latitude = side < 0 ? 1 - HemisphereRings + ring : ring;
                    if (side > 0 && latitude == 0 && halfHeight == 0) continue;
                    double angle = latitude * (Math.PI / 2 / HemisphereRings);
                    float x = side * halfHeight + radius * (float)Math.Sin(angle);
                    float radial = radius * (float)Math.Cos(angle);
                    for (int j = 0; j < Segments; j++)
                    {
                        double azimuth = j * (2 * Math.PI / Segments);
                        p.Add(new Vector3(x, radial * (float)Math.Cos(azimuth), radial * (float)Math.Sin(azimuth)));
                    }
                }
            }
            int pole = p.Count;
            p.Add(new Vector3(halfHeight + radius, 0, 0));
            points = p.ToArray();
            if (wire)
            {
                indices = RoundedWireIndices(pole);
                return;
            }
            var f = new List<int>();
            for (int j = 0; j < Segments; j++)
            {
                int next = (j + 1) % Segments;
                Triangle(f, 0, 1 + next, 1 + j);
                for (int row = 1; row < pole - Segments; row += Segments)
                {
                    int a = row + j, b = row + next;
                    Triangle(f, a, b, b + Segments);
                    Triangle(f, a, b + Segments, a + Segments);
                }
                Triangle(f, pole - Segments + j, pole - Segments + next, pole);
            }
            indices = f.ToArray();
        }

        private static int[] RoundedWireIndices(int pole)
        {
            var lines = new List<int>();
            int leftRim = 1 + (HemisphereRings - 1) * Segments;
            int rightRim = pole - HemisphereRings * Segments;
            for (int rim = leftRim; rim <= rightRim; rim += Segments)
            {
                for (int j = 0; j < Segments; j++)
                {
                    lines.Add(rim + j);
                    lines.Add(rim + (j + 1) % Segments);
                }
            }
            for (int j = 0; j < Segments; j += Segments / 4)
            {
                int previous = 0;
                for (int vertex = 1 + j; vertex < pole; vertex += Segments)
                {
                    lines.Add(previous);
                    lines.Add(vertex);
                    previous = vertex;
                }
                lines.Add(previous);
                lines.Add(pole);
            }
            return lines.ToArray();
        }
    }
}
