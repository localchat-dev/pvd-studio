using System;
using System.Threading.Tasks;

namespace PvdStudio.Editor
{
    internal readonly struct FrameCrop : IEquatable<FrameCrop>
    {
        internal const uint WorldMode = 1;
        internal const uint ActorMode = 2;

        internal readonly uint Mode;
        internal readonly ulong ActorId;
        internal readonly uint ActorGeneration;
        internal readonly float X, Y, Z, Radius;

        private FrameCrop(uint mode, ulong actorId, uint actorGeneration, float x, float y, float z, float radius)
        {
            Mode = mode;
            ActorId = actorId;
            ActorGeneration = actorGeneration;
            X = x;
            Y = y;
            Z = z;
            Radius = radius;
        }

        internal bool Enabled => Mode != 0;

        internal static FrameCrop Actor(ulong id, uint generation, float radius) =>
            new FrameCrop(ActorMode, id, generation, 0, 0, 0, radius);

        internal static bool TryWorld(float x, float y, float z, bool mirrorX, float radius, out FrameCrop crop)
        {
            crop = default;
            if (!IsFinite(x) || !IsFinite(y) || !IsFinite(z) || !IsRadiusValid(radius)) return false;
            crop = new FrameCrop(WorldMode, 0, 0, mirrorX ? -x : x, y, z, radius);
            return true;
        }

        internal static bool IsRadiusValid(float radius) => radius > 0 && IsFinite(radius);

        private static bool IsFinite(float value) => !float.IsNaN(value) && !float.IsInfinity(value);

        public bool Equals(FrameCrop other) => Mode == other.Mode && ActorId == other.ActorId &&
            ActorGeneration == other.ActorGeneration && X.Equals(other.X) && Y.Equals(other.Y) &&
            Z.Equals(other.Z) && Radius.Equals(other.Radius);

        public override bool Equals(object obj) => obj is FrameCrop other && Equals(other);

        public override int GetHashCode() => (Mode, ActorId, ActorGeneration, X, Y, Z, Radius).GetHashCode();
    }

    internal sealed class FrameWorker
    {
        internal sealed class Result
        {
            internal long Index;
            internal FrameCrop Crop;
            internal bool IncludesRays;
            internal bool IncludesQueries;
            internal SurfaceFrameSnapshot Frame;
            internal Exception QueryError;
            internal Exception Error;
        }

        private Task<Result> active;
        private bool obsolete;

        // The editor presents its newest target on each update. No history of skipped targets is queued.
        internal bool Start(long index, Func<SurfaceFrameSnapshot> fetch, FrameCrop crop = default,
            bool includeRays = false, bool includeQueries = false, Func<SurfaceFrameSnapshot> fetchWithoutQueries = null)
        {
            if (active != null) return false;
            obsolete = false;
            active = Task.Run(() => {
                var result = new Result {
                    Index = index, Crop = crop, IncludesRays = includeRays, IncludesQueries = includeQueries
                };
                try { result.Frame = fetch(); }
                catch (EntryPointNotFoundException e) when (includeQueries && fetchWithoutQueries != null)
                {
                    result.QueryError = e;
                    try { result.Frame = fetchWithoutQueries(); }
                    catch (Exception fallbackError) { result.Error = fallbackError; }
                }
                catch (Exception e) { result.Error = e; }
                return result;
            });
            return true;
        }

        internal void Invalidate() { obsolete = true; }

        internal bool Busy => active != null && !active.IsCompleted;

        internal Result Take()
        {
            if (active == null || !active.IsCompleted) return null;
            var result = active.GetAwaiter().GetResult();
            active = null;
            return obsolete ? null : result;
        }

        // Only assembly unload and editor exit may wait for native memory to be returned.
        internal void Wait() { active?.GetAwaiter().GetResult(); }
    }
}
