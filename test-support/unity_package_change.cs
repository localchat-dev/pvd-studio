using System;
using System.Collections.Generic;
using System.IO;
using System.Net;
using System.Net.Sockets;
using System.Runtime.InteropServices;
using System.Text.Json;
using System.Threading;
using PvdStudio.Editor;
using UnityEditor.PackageManager;

// The harness supplies editor notifications while loading the packaged bridge and real sockets.
namespace UnityEditor.PackageManager
{
    public class PackageInfo { public string name; }
    public class PackageRegistrationEventArgs
    {
        public IEnumerable<PackageInfo> removed = Array.Empty<PackageInfo>();
        public IEnumerable<PackageInfo> changedFrom = Array.Empty<PackageInfo>();
    }
    public static class Events
    {
        public static event Action<PackageRegistrationEventArgs> registeringPackages;
        public static void Raise(PackageRegistrationEventArgs args) => registeringPackages?.Invoke(args);
    }
}
namespace UnityEditor
{
    public class InitializeOnLoadAttribute : Attribute { }
    public static class AssemblyReloadEvents { public static Action beforeAssemblyReload; }
    public static class EditorApplication { public static Action quitting, update; }
    public static class SessionState
    {
        private static readonly Dictionary<string, bool> values = new Dictionary<string, bool>();
        public static void SetBool(string key, bool value) => values[key] = value;
        public static bool GetBool(string key, bool fallback) => values.TryGetValue(key, out var v) ? v : fallback;
    }
}
namespace UnityEngine
{
    public static class Application { public static string temporaryCachePath; }
    public static class Debug { public static void LogWarning(string message) => Console.Error.WriteLine(message); }
    public static class JsonUtility
    {
        private static readonly JsonSerializerOptions options = new JsonSerializerOptions { IncludeFields = true };
        public static string LastRequest { get; private set; }
        public static string ToJson(object value) => LastRequest = JsonSerializer.Serialize(value, value.GetType(), options);
        public static T FromJson<T>(string text) => JsonSerializer.Deserialize<T>(text, options);
    }
}
namespace PvdStudio.Editor
{
    internal readonly struct SurfaceFrameSnapshot
    {
        internal readonly uint Magic;
        internal SurfaceFrameSnapshot(uint magic) { Magic = magic; }
    }
    internal class SurfaceFrameReader
    {
        public const int MaxBytes = 32 * 1024 * 1024;
        public ulong Token => 0;
        public SurfaceFrameSnapshot DecodeSnapshot(byte[] data, bool expectRays, bool expectQueries = false)
            => new SurfaceFrameSnapshot(BitConverter.ToUInt32(data, 0));
    }
}

public static class PackageChangeProbe
{
    private static long SendFrames(TcpClient peer, bool recordHistory, int count)
    {
        using (var writer = new BinaryWriter(peer.GetStream(), System.Text.Encoding.UTF8, true))
        {
            writer.Write(837150850u);
            writer.Write(1u);
            writer.Write(1ul);
            writer.Write(10ul);
            writer.Write(1u);
            for (int i = 0; i < count; i++)
            {
                using var group = new MemoryStream();
                using var events = new BinaryWriter(group);
                events.Write((byte)1);
                events.Write(6u);
                events.Write(System.Text.Encoding.ASCII.GetBytes("frame\0"));
                events.Write(1u);
                events.Write((byte)19);
                events.Write(7ul);
                events.Write(1u);
                events.Write((ulong)i);
                writer.Write((uint)group.Length);
                writer.Write(2u);
                writer.Write(3ul);
                writer.Write((ulong)i);
                writer.Write(group.ToArray());
            }
        }
        ViewerSession session = null;
        if (!SpinWait.SpinUntil(() => {
            session = NativeViewer.Command("status").status.sessions[0];
            return session.frames == count;
        }, 5000))
            throw new IOException("The test frames were not published.");
        if (session.liveOnly == recordHistory)
            throw new IOException("The native viewing mode differs from the managed start request.");
        return session.id;
    }

    private static void VerifyLatestFrame(TcpClient peer)
    {
        long id = SendFrames(peer, recordHistory: false, count: 2);
        FrameCrop.TryWorld(0, 0, 0, false, 10, out var crop);
        if (!NativeViewer.RequestFrame(id, 0, 0, crop, includeRays: true, includeQueries: true, latestOnly: true))
            throw new IOException("The latest-frame request did not start.");
        FrameWorker.Result frame = null;
        if (!SpinWait.SpinUntil(() => (frame = NativeViewer.TakeFrame()) != null, 5000))
            throw new IOException("The latest-frame request did not finish.");
        if (frame.Error != null || frame.QueryError != null)
            throw new IOException("The latest-frame request failed.", frame.Error ?? frame.QueryError);
        if (frame.Index != 0 || !frame.Crop.Equals(crop) || frame.Frame.Magic != 0x35535650u)
            throw new IOException("The latest query frame was not returned with its original editor request metadata.");
    }

    private static void ExportDuringReload(TcpClient peer, string path)
    {
        long id = SendFrames(peer, recordHistory: true, count: 1);
        NativeViewer.Command("export", session: id, path: path);
        UnityEditor.AssemblyReloadEvents.beforeAssemblyReload?.Invoke();
        var status = NativeViewer.Command("status").status;
        if (!status.listening || status.export.running || status.export.session != id)
            throw new IOException("Reload failed to drain the export and retain the listener.");
        if (!String.IsNullOrEmpty(status.export.error) &&
            status.export.error.IndexOf("cancel", StringComparison.OrdinalIgnoreCase) < 0)
            throw new IOException("Unexpected export error: " + status.export.error);
        if (File.Exists(path) != String.IsNullOrEmpty(status.export.error))
            throw new IOException("The export result and published file disagree after reload.");
    }

    public static void Run(string dll, string change)
    {
        NativeLibrary.SetDllImportResolver(typeof(NativeViewer).Assembly,
            (name, assembly, path) => name == "pvd_unity" ? NativeLibrary.Load(dll) : IntPtr.Zero);
        var temp = Directory.CreateTempSubdirectory("pvd-package-test-");
        UnityEngine.Application.temporaryCachePath = temp.FullName;
        try
        {
            bool recordHistory = change == "reload";
            if (recordHistory) NativeViewer.Command("start", "127.0.0.1:0", recordHistory: true);
            else NativeViewer.Command("start", "127.0.0.1:0");
            using (var request = JsonDocument.Parse(UnityEngine.JsonUtility.LastRequest))
                if (!request.RootElement.TryGetProperty("recordHistory", out var history) || history.GetBoolean() != recordHistory)
                    throw new IOException("The listener must request the latest frame by default and accept explicit recording.");
            var address = NativeViewer.Command("status").status.address;
            var endpoint = IPEndPoint.Parse(address);
            using (var peer = new TcpClient())
            {
                peer.Connect(endpoint);
                if (!SpinWait.SpinUntil(() => NativeViewer.Command("status").status.sessions.Length > 0, 5000))
                    throw new IOException("Listener did not accept the test client.");
                NativeViewer.Release();
                Events.Raise(new PackageRegistrationEventArgs {
                    removed = new[] { new PackageInfo { name = "com.example.unrelated" } }
                });
                if (!NativeViewer.Command("status").status.listening)
                    throw new IOException("Unrelated package change stopped the receiver.");
                if (change == "reload")
                {
                    ExportDuringReload(peer, Path.Combine(temp.FullName, "reload.pvdcapture"));
                    UnityEditor.EditorApplication.quitting?.Invoke();
                }
                else
                {
                    VerifyLatestFrame(peer);
                    var viewer = new[] { new PackageInfo { name = "com.pvdstudio.viewer" } };
                    var args = new PackageRegistrationEventArgs();
                    if (change == "remove") args.removed = viewer;
                    else args.changedFrom = viewer;
                    Events.Raise(args);
                }
                UnityEditor.AssemblyReloadEvents.beforeAssemblyReload?.Invoke();
                if (NativeViewer.Command("status").status.listening)
                    throw new IOException("Old DLL is still listening after package " + change);
                peer.ReceiveTimeout = 3000;
                if (peer.GetStream().ReadByte() != -1)
                    throw new IOException("Expected the old connection to close.");
            }
            var replacement = new TcpListener(endpoint);
            try { replacement.Start(); }
            finally { replacement.Stop(); }
            Console.WriteLine("PASS " + change + ": connected peer closed and replacement bound " + address);
        }
        finally
        {
            NativeViewer.Command("shutdown");
            temp.Delete(true);
        }
    }
}
