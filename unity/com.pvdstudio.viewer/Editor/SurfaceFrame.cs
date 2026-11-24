using System;
using System.Collections.Generic;
using System.IO;
using MeshKey = System.ValueTuple<ulong, ulong, uint, ulong, uint>;
using UnityEngine;

namespace PvdStudio.Editor
{
    internal sealed class SurfaceRecord
    {
        internal ulong RoomNumber, ActorId, ShapeId;
        internal uint ActorGeneration, ShapeGeneration;
        internal byte State;
        internal Vector3 ActorPosition, LocalPosition;
        internal Quaternion ActorRotation, LocalRotation;
        internal SurfaceGeometry Geometry;
    }

    internal readonly struct RayRecord
    {
        internal readonly Vector3 Start, End, Marker;
        internal readonly bool Hit, HasMarker;

        internal RayRecord(Vector3 start, Vector3 end, Vector3 marker, bool hit, bool hasMarker)
        {
            Start = start;
            End = end;
            Marker = marker;
            Hit = hit;
            HasMarker = hasMarker;
        }
    }

    internal readonly struct QueryLine
    {
        internal readonly Vector3 Start, End;
        internal readonly byte Style;

        internal QueryLine(Vector3 start, Vector3 end, byte style)
        {
            Start = start;
            End = end;
            Style = style;
        }
    }

    internal readonly struct SurfaceFrameSnapshot
    {
        internal readonly SurfaceRecord[] Records;
        internal readonly RayRecord[] Rays;
        internal readonly QueryLine[] Queries;
        internal readonly bool RaysTruncated;
        internal readonly bool QueriesTruncated;

        internal SurfaceFrameSnapshot(SurfaceRecord[] records, RayRecord[] rays, bool raysTruncated)
            : this(records, rays, raysTruncated, Array.Empty<QueryLine>(), false) { }

        internal SurfaceFrameSnapshot(SurfaceRecord[] records, RayRecord[] rays, bool raysTruncated,
            QueryLine[] queries, bool queriesTruncated)
        {
            Records = records;
            Rays = rays;
            RaysTruncated = raysTruncated;
            Queries = queries;
            QueriesTruncated = queriesTruncated;
        }
    }

    internal static class SurfaceFrame
    {
        internal static SurfaceRecord[] Decode(byte[] frame) => new SurfaceFrameReader().Decode(frame);
    }

    internal sealed class SurfaceFrameReader
    {
        private readonly object gate = new object();
        private Dictionary<MeshKey, SurfaceGeometry> legacyMeshes;
        private Dictionary<ulong, SurfaceGeometry> resources;
        private ulong token;
        internal ulong Token { get { lock (gate) return token; } }

        internal const int MaxBytes = 32 * 1024 * 1024;
        private const long MaxResourceBytes = 128L * 1024 * 1024;
        private const int FixedRecordBytes = 90;
        private const int MaxRays = 2048;
        private const int RayBytes = 37;
        internal const int MaxQueryLines = 8192;
        private const int QueryLineBytes = 25;

        internal SurfaceRecord[] Decode(byte[] frame) => DecodeSnapshot(frame).Records;

        internal SurfaceFrameSnapshot DecodeSnapshot(byte[] frame, bool expectRays = false, bool expectQueries = false)
        {
            lock (gate) return DecodeFrame(frame, expectRays, expectQueries);
        }

        private SurfaceFrameSnapshot DecodeFrame(byte[] frame, bool expectRays, bool expectQueries)
        {
            if (frame == null || frame.Length < 8 || frame.Length > MaxBytes)
                throw new InvalidDataException("Invalid PVD surface frame size.");
            using (var stream = new MemoryStream(frame, false))
            using (var reader = new BinaryReader(stream))
            {
                uint magic = reader.ReadUInt32();
                ulong nextToken = 0, previous = 0;
                bool queryFrame = magic == 0x35535650; // PVS5
                bool rayFrame = queryFrame || magic == 0x34535650; // PVS4
                bool resourceFrame = rayFrame || magic == 0x33535650; // PVS3
                bool cachedFrame = magic == 0x32535650; // PVS2
                if (resourceFrame || cachedFrame)
                {
                    Require(reader, 20);
                    nextToken = reader.ReadUInt64();
                    previous = reader.ReadUInt64();
                    bool hasBase = resourceFrame ? resources != null : legacyMeshes != null;
                    if (nextToken == 0 || nextToken == previous || (previous != 0 && (previous != token || !hasBase)))
                        throw new InvalidDataException("Invalid PVD surface frame token.");
                }
                else if (magic != 0x31535650) // PVS1
                    throw new InvalidDataException("Unsupported PVD surface frame format.");
                var nextLegacy = cachedFrame ? new Dictionary<MeshKey, SurfaceGeometry>() : null;
                var nextResources = resourceFrame ? new Dictionary<ulong, SurfaceGeometry>() : null;
                var records = resourceFrame
                    ? ReadResourceFrame(reader, previous == 0 ? null : resources, nextResources)
                    : ReadLegacyFrame(reader, previous == 0 ? null : legacyMeshes, nextLegacy);
                // When the overlays cannot fit, the native view may return only the PVS3 surfaces.
                bool raysTruncated = expectRays && resourceFrame && !rayFrame;
                bool queriesTruncated = expectQueries && resourceFrame && !queryFrame;
                var rays = rayFrame ? ReadRays(reader, out raysTruncated) : Array.Empty<RayRecord>();
                var queries = queryFrame ? ReadQueries(reader, out queriesTruncated) : Array.Empty<QueryLine>();
                if (stream.Position != stream.Length)
                    throw new InvalidDataException("Trailing bytes in PVD surface frame.");
                resources = nextResources;
                legacyMeshes = nextLegacy;
                token = nextToken;
                return new SurfaceFrameSnapshot(records, rays, raysTruncated, queries, queriesTruncated);
            }
        }

        private static RayRecord[] ReadRays(BinaryReader reader, out bool truncated)
        {
            Require(reader, 8);
            uint flags = reader.ReadUInt32();
            uint count = reader.ReadUInt32();
            if ((flags & ~1u) != 0 || count > MaxRays)
                throw new InvalidDataException("Invalid PVD ray section.");
            truncated = (flags & 1) != 0;
            Require(reader, RayBytes * (long)count);
            if (count == 0) return Array.Empty<RayRecord>();

            var rays = new RayRecord[(int)count];
            for (int i = 0; i < rays.Length; i++)
            {
                byte rayFlags = reader.ReadByte();
                if (rayFlags > 3 || rayFlags == 2)
                    throw new InvalidDataException("Invalid PVD ray flags.");
                var start = Vector(reader);
                var end = Vector(reader);
                var marker = Vector(reader);
                bool hasMarker = (rayFlags & 2) != 0;
                if (!hasMarker && (marker.x != 0 || marker.y != 0 || marker.z != 0))
                    throw new InvalidDataException("Invalid PVD ray marker.");
                rays[i] = new RayRecord(start, end, marker, (rayFlags & 1) != 0, hasMarker);
            }
            return rays;
        }

        private static QueryLine[] ReadQueries(BinaryReader reader, out bool truncated)
        {
            Require(reader, 8);
            uint flags = reader.ReadUInt32();
            uint count = reader.ReadUInt32();
            if ((flags & ~1u) != 0 || count > MaxQueryLines)
                throw new InvalidDataException("Invalid PVD shape query section.");
            truncated = (flags & 1) != 0;
            Require(reader, QueryLineBytes * (long)count);
            if (count == 0) return Array.Empty<QueryLine>();

            var lines = new QueryLine[(int)count];
            for (int i = 0; i < lines.Length; i++)
            {
                byte style = reader.ReadByte();
                if (style > 2) throw new InvalidDataException("Invalid PVD shape query style.");
                lines[i] = new QueryLine(Vector(reader), Vector(reader), style);
            }
            return lines;
        }

        private static SurfaceRecord[] ReadResourceFrame(BinaryReader reader,
            Dictionary<ulong, SurfaceGeometry> basis, Dictionary<ulong, SurfaceGeometry> next)
        {
            uint definitionCount = reader.ReadUInt32();
            // Every definition needs its own ID and counts, plus at least one shape referencing it.
            Require(reader, 4 + (16L + FixedRecordBytes + 8) * definitionCount);
            var pending = new Dictionary<ulong, SurfaceGeometry>();
            for (uint i = 0; i < definitionCount; i++)
            {
                Require(reader, 8);
                ulong id = reader.ReadUInt64();
                if (id == 0 || pending.ContainsKey(id) || (basis != null && basis.ContainsKey(id)))
                    throw new InvalidDataException("Invalid PVD mesh definition ID.");
                pending.Add(id, ReadGeometry(reader, 4));
            }

            long retainedBytes = 0;
            var records = ReadRecords(reader, (record, kind) =>
            {
                if (kind != 4) return ReadGeometry(reader, kind);
                Require(reader, 8);
                ulong id = reader.ReadUInt64();
                if (next.TryGetValue(id, out var mesh)) return mesh;
                if (pending.TryGetValue(id, out mesh)) pending.Remove(id);
                else if (basis == null || !basis.TryGetValue(id, out mesh))
                    throw new InvalidDataException("Missing PVD mesh resource.");
                retainedBytes += mesh.MeshPayloadBytes;
                if (retainedBytes > MaxResourceBytes)
                    throw new InvalidDataException("Retained PVD mesh resources exceed size limit.");
                next.Add(id, mesh);
                return mesh;
            });
            if (pending.Count != 0)
                throw new InvalidDataException("Unreferenced PVD mesh definition.");
            return records;
        }

        private static SurfaceRecord[] ReadLegacyFrame(BinaryReader reader,
            Dictionary<MeshKey, SurfaceGeometry> basis, Dictionary<MeshKey, SurfaceGeometry> next)
        {
            long expanded = reader.BaseStream.Length;
            return ReadRecords(reader, (record, kind) =>
            {
                var key = new MeshKey(record.RoomNumber, record.ActorId, record.ActorGeneration,
                    record.ShapeId, record.ShapeGeneration);
                SurfaceGeometry mesh;
                if (kind == 5)
                {
                    if (basis == null || !basis.TryGetValue(key, out mesh))
                        throw new InvalidDataException("Missing PVD surface mesh definition.");
                    expanded += mesh.MeshPayloadBytes;
                    if (expanded > MaxBytes)
                        throw new InvalidDataException("Expanded PVD surface frame exceeds size limit.");
                }
                else mesh = ReadGeometry(reader, kind);
                if (next != null && mesh.Kind == 4) next[key] = mesh;
                return mesh;
            });
        }

        private static SurfaceRecord[] ReadRecords(BinaryReader reader, Func<SurfaceRecord, byte, SurfaceGeometry> geometry)
        {
            Require(reader, 4);
            uint count = reader.ReadUInt32();
            Require(reader, FixedRecordBytes * (long)count);
            var records = new SurfaceRecord[(int)count];
            for (int i = 0; i < records.Length; i++)
            {
                Require(reader, FixedRecordBytes);
                var record = new SurfaceRecord
                {
                    RoomNumber = reader.ReadUInt64(),
                    ActorId = reader.ReadUInt64(),
                    ActorGeneration = reader.ReadUInt32(),
                    ShapeId = reader.ReadUInt64(),
                    ShapeGeneration = reader.ReadUInt32(),
                    State = reader.ReadByte()
                };
                byte kind = reader.ReadByte();
                if (record.State > 2)
                    throw new InvalidDataException("Invalid PVD body state.");
                record.ActorPosition = Vector(reader);
                record.ActorRotation = Rotation(reader);
                record.LocalPosition = Vector(reader);
                record.LocalRotation = Rotation(reader);
                record.Geometry = geometry(record, kind);
                records[i] = record;
            }
            return records;
        }

        private static SurfaceGeometry ReadGeometry(BinaryReader reader, byte kind)
        {
            var size = Vector3.zero;
            switch (kind)
            {
                case 0:
                    Require(reader, 12);
                    size = Vector(reader);
                    break;
                case 1:
                    Require(reader, 4);
                    size.x = Scalar(reader);
                    break;
                case 2:
                    Require(reader, 8);
                    size.x = Scalar(reader);
                    size.y = Scalar(reader);
                    break;
                case 3:
                    break;
                case 4:
                    Require(reader, 8);
                    uint vertexCount = reader.ReadUInt32();
                    uint indexCount = reader.ReadUInt32();
                    Require(reader, 12L * vertexCount + 4L * indexCount);
                    if (indexCount % 3 != 0)
                        throw new InvalidDataException("Incomplete PVD mesh triangle.");
                    var vertices = new Vector3[(int)vertexCount];
                    var indices = new int[(int)indexCount];
                    for (int i = 0; i < vertices.Length; i++) vertices[i] = Vector(reader);
                    for (int i = 0; i < indices.Length; i++)
                    {
                        uint index = reader.ReadUInt32();
                        if (index >= vertexCount)
                            throw new InvalidDataException("PVD triangle index outside vertex array.");
                        indices[i] = (int)index;
                    }
                    return new SurfaceGeometry(kind, size, vertices, indices);
                default:
                    throw new InvalidDataException("Unsupported PVD surface geometry.");
            }
            if (size.x < 0 || size.y < 0 || size.z < 0)
                throw new InvalidDataException("Negative PVD primitive dimension.");
            return new SurfaceGeometry(kind, size);
        }

        private static void Require(BinaryReader reader, long bytes)
        {
            if (bytes > reader.BaseStream.Length - reader.BaseStream.Position)
                throw new InvalidDataException("Incomplete PVD surface frame.");
        }

        private static float Scalar(BinaryReader reader)
        {
            float value = reader.ReadSingle();
            if (float.IsNaN(value) || float.IsInfinity(value))
                throw new InvalidDataException("Non-finite PVD surface value.");
            return value;
        }

        private static Vector3 Vector(BinaryReader r) => new Vector3(Scalar(r), Scalar(r), Scalar(r));
        private static Quaternion Rotation(BinaryReader r)
        {
            var q = new Quaternion(Scalar(r), Scalar(r), Scalar(r), Scalar(r));
            float lengthSquared = q.x * q.x + q.y * q.y + q.z * q.z + q.w * q.w;
            // The native writer normalizes captured rotations before encoding them.
            if (System.Math.Abs(lengthSquared - 1f) > 0.001f)
                throw new InvalidDataException("Invalid PVD surface rotation.");
            return q;
        }
    }
}
