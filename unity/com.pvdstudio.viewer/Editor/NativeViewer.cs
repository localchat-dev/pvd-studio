using System;
using System.IO;
using System.Linq;
using System.Runtime.InteropServices;
using System.Text;
using UnityEditor;
using UnityEditor.PackageManager;
using UnityEngine;

namespace PvdStudio.Editor
{
    [Serializable]
    internal sealed class ViewerRoom
    {
        public long number;
    }

    [Serializable]
    internal sealed class ViewerSession
    {
        public long id;
        public long frames;
        public bool active;
        public bool liveOnly;
        public ViewerRoom[] rooms;
    }

    [Serializable]
    internal sealed class SessionExport
    {
        public long session;
        public string path;
        public bool running;
        public string error;
        public long frames;
        public long bytes;
    }

    [Serializable]
    internal sealed class ViewerStatus
    {
        public bool listening;
        public string address;
        public string sharedFrom;
        public bool waitingForSession;
        public bool takingOver;
        public string error;
        public string frameError;
        public SessionExport export;
        public long totalBytes;
        public long clearableBytes;
        public ViewerSession[] sessions;
    }

    [InitializeOnLoad]
    internal static class NativeViewer
    {
        private const string Library = "pvd_unity";
        private const string Loaded = "PvdStudio.NativeViewer.Loaded";

        [Serializable]
        private class Request
        {
            public string op;
            public string address;
            public string root;
            public long session;
            public string path;
            public bool recordHistory;
            public bool allowLateJoin;
        }

        [Serializable]
        internal class Reply
        {
            public bool ok;
            public string error;
            public ViewerStatus status;
            public long released;
        }

        [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
        private static extern IntPtr pvd_request([In] byte[] input);

        [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
        private static extern void pvd_free(IntPtr json);

        [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
        private static extern IntPtr pvd_frame_resources(ulong id, ulong frame, ulong room, ulong baseToken, out int length);

        [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
        private static extern IntPtr pvd_frame_resources_near(ulong id, ulong frame, ulong room, ulong baseToken,
            uint mode, ulong actor, uint generation, float x, float y, float z, float radius, out int length);

        [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
        private static extern IntPtr pvd_frame_view(ulong id, ulong frame, ulong room, ulong baseToken,
            uint mode, ulong actor, uint generation, float x, float y, float z, float radius, out int length);

        [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
        private static extern IntPtr pvd_frame_queries(ulong id, ulong frame, ulong room, ulong baseToken,
            uint mode, ulong actor, uint generation, float x, float y, float z, float radius,
            uint includeRays, out int length);

        [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
        internal static extern long pvd_frame_count(ulong id);

        [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
        private static extern void pvd_frame_free(IntPtr data, int length);

        private static readonly FrameWorker worker = new FrameWorker();
        private static SurfaceFrameReader reader;
        private static bool releasePending;
        private static bool unloading;
        private static bool viewRequested;
        private static bool exportRequested;

        static NativeViewer()
        {
            AssemblyReloadEvents.beforeAssemblyReload += Drain;
            Events.registeringPackages += BeforePackageChange;
            EditorApplication.quitting += Shutdown;
        }

        private static void BeforePackageChange(PackageRegistrationEventArgs change)
        {
            const string package = "com.pvdstudio.viewer";
            if (change.changedFrom.Any(p => p.name == package) || change.removed.Any(p => p.name == package))
                Shutdown();
        }

        private static void Shutdown()
        {
            Drain();
            if (!SessionState.GetBool(Loaded, false)) return;
            try { Command("shutdown"); }
            catch (Exception e) { Debug.LogWarning("PVD Studio: " + e.Message); }
        }

        internal static void Release()
        {
            ResetFrameReader();
            if (unloading || releasePending) return;
            releasePending = true;
            EditorApplication.update += FinishRelease;
            FinishRelease();
        }

        private static void FinishRelease()
        {
            if (!releasePending || worker.Busy) return;
            EditorApplication.update -= FinishRelease;
            releasePending = false;
            viewRequested = false;
            worker.Take();
            try { Command("release"); }
            catch (Exception e) { Debug.LogWarning("PVD Studio: " + e.Message); }
        }

        private static void Drain()
        {
            // A scheduled fetch can still enter native code. Release its pin after it exits.
            unloading = true;
            worker.Invalidate();
            worker.Wait();
            worker.Take();
            if (viewRequested || releasePending)
            {
                releasePending = true;
                FinishRelease();
            }
            reader = null;
            if (exportRequested)
            {
                try { Command("drainExport"); }
                catch (Exception e) { Debug.LogWarning("PVD Studio: " + e.Message); }
                exportRequested = false;
            }
        }

        internal static void InvalidateFrame() { worker.Invalidate(); }

        internal static void ResetFrameReader()
        {
            worker.Invalidate();
            reader = null;
        }

        internal static FrameWorker.Result TakeFrame() => worker.Take();

        internal static bool RequestFrame(long id, long index, long room, FrameCrop crop = default,
            bool includeRays = false, bool includeQueries = false, bool latestOnly = false)
        {
            if (unloading || releasePending || worker.Busy) return false;
            var decoder = reader ?? (reader = new SurfaceFrameReader());
            SurfaceFrameSnapshot Fetch(bool rays, bool queries) => decoder.DecodeSnapshot(
                Frame(id, index, room, decoder.Token, crop, rays, queries, latestOnly), rays, queries);
            Func<SurfaceFrameSnapshot> fallback = includeQueries ? () => FetchWithoutQueries(Fetch, includeRays) : null;
            bool started = worker.Start(index, () => Fetch(includeRays, includeQueries), crop, includeRays, includeQueries, fallback);
            if (started) viewRequested = true;
            return started;
        }

        internal static SurfaceFrameSnapshot FetchWithoutQueries(Func<bool, bool, SurfaceFrameSnapshot> fetch, bool includeRays)
        {
            try { return fetch(includeRays, false); }
            catch (EntryPointNotFoundException) when (includeRays) { return fetch(false, false); }
        }

        internal static Reply Command(string op, string address = null, long session = 0, string path = null,
            bool recordHistory = false, bool allowLateJoin = true)
        {
            if (unloading && (op == "start" || op == "export"))
                throw new InvalidOperationException("插件正在重载，请在完成后重试。");
            var request = new Request {
                op = op, address = address, session = session, path = path, recordHistory = recordHistory,
                allowLateJoin = allowLateJoin,
                root = Path.Combine(Application.temporaryCachePath, "PvdStudio")
            };
            byte[] input = Encoding.UTF8.GetBytes(JsonUtility.ToJson(request) + "\0");
            IntPtr json = pvd_request(input);
            if (json == IntPtr.Zero) throw new InvalidDataException("原生桥接未返回状态。");
            try
            {
                SessionState.SetBool(Loaded, true);
                int length = 0;
                while (Marshal.ReadByte(json, length) != 0) length = checked(length + 1);
                var data = new byte[length];
                Marshal.Copy(json, data, 0, length);
                var result = JsonUtility.FromJson<Reply>(Encoding.UTF8.GetString(data));
                if (result == null || !result.ok)
                    throw new InvalidOperationException(result?.error ?? "无法读取原生桥接响应。");
                if (op == "export") exportRequested = true;
                if (op == "shutdown") SessionState.SetBool(Loaded, false);
                return result;
            }
            finally { pvd_free(json); }
        }

        private static byte[] Frame(long id, long index, long room, ulong baseToken, FrameCrop crop,
            bool includeRays, bool includeQueries, bool latestOnly)
        {
            ulong nativeId = checked((ulong)id);
            // Keep the editor's published frame index on the worker result while native reads its latest snapshot.
            ulong nativeIndex = latestOnly ? ulong.MaxValue : checked((ulong)index);
            ulong nativeRoom = checked((ulong)room);
            int length;
            IntPtr frame;
            if (includeQueries)
                frame = pvd_frame_queries(nativeId, nativeIndex, nativeRoom, baseToken, crop.Mode,
                    crop.ActorId, crop.ActorGeneration, crop.X, crop.Y, crop.Z, crop.Radius, includeRays ? 1u : 0u, out length);
            else if (includeRays)
                frame = pvd_frame_view(nativeId, nativeIndex, nativeRoom, baseToken, crop.Mode,
                    crop.ActorId, crop.ActorGeneration, crop.X, crop.Y, crop.Z, crop.Radius, out length);
            else if (crop.Enabled)
                frame = pvd_frame_resources_near(nativeId, nativeIndex, nativeRoom, baseToken, crop.Mode,
                    crop.ActorId, crop.ActorGeneration, crop.X, crop.Y, crop.Z, crop.Radius, out length);
            else
                frame = pvd_frame_resources(nativeId, nativeIndex, nativeRoom, baseToken, out length);
            if (frame == IntPtr.Zero)
                throw new InvalidOperationException("无法读取所选帧。");
            try
            {
                if (length <= 0 || length > SurfaceFrameReader.MaxBytes)
                    throw new InvalidDataException("原生桥接返回了无效的帧大小。");
                var data = new byte[length];
                Marshal.Copy(frame, data, 0, length);
                return data;
            }
            finally { pvd_frame_free(frame, length); }
        }
    }
}
