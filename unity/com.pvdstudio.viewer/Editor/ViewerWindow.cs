using System;
using UnityEditor;
using UnityEngine;

namespace PvdStudio.Editor
{
    internal sealed class ViewerWindow : EditorWindow
    {
        private static ViewerWindow owner;
        private static readonly string[] DisplayModes = { "面", "线框" };
        private const float DefaultCropRadius = 20f;
        [SerializeField] private string address = "0.0.0.0:5425";
        [SerializeField] private bool recordHistory;
        [SerializeField] private long sessionId;
        [SerializeField] private long room;
        [SerializeField] private bool autoSelect = true;
        [SerializeField] private bool mirrorX;
        [SerializeField] private bool wireframe;
        [SerializeField] private bool showInGameView;
        [SerializeField] private bool showPhysicsQueries = true;
        [SerializeField] private bool restorePaused;
        [SerializeField] private long restoreFrame;
        [SerializeField] private int playbackRate = 15;
        [SerializeField] private float cropRadius = DefaultCropRadius;
        [SerializeField] private bool cropEnabled;
        [SerializeField] private Transform cropTransform;
        [SerializeField] private long cropSession;
        [SerializeField] private ulong cropActor;
        [SerializeField] private uint cropActorGeneration;
        [SerializeField] private string cropLabel;

        private PlaybackCursor cursor;
        private ViewerStatus status;
        private double nextPoll;
        private double nextLivePoll;
        private bool selectedActive;
        private bool selectedLiveOnly;
        private double nextStep;
        private double retryFrameAt;
        private double nextWorldCropRequest;
        private int direction;
        private long displayed = -1;
        private FrameCrop displayedCrop;
        private bool displayedWithPhysicsQueries;
        private bool raysTruncated;
        private bool queriesTruncated;
        private long requestedFrame = -1;
        private FrameCrop requestedCrop;
        private long released;
        private string commandError;
        private string pollError;
        private string frameError;
        private string rayError;
        private string queryError;
        private string cropUnavailable;
        private Vector2 scroll;

        [MenuItem("Tools/PVD Studio")]
        private static void Open()
        {
            GetWindow<ViewerWindow>("PVD Studio");
        }

        private void OnEnable()
        {
            // One native view pin and one Scene overlay are shared by all editor tabs.
            if (owner != null && owner != this)
            {
                EditorApplication.delayCall += CloseDuplicate;
                return;
            }
            owner = this;
            cursor = new PlaybackCursor();
            SceneOverlay.MirrorX = mirrorX;
            SceneOverlay.Wireframe = wireframe;
            SceneOverlay.ShowInGameView = showInGameView;
            if (!FrameCrop.IsRadiusValid(cropRadius)) cropRadius = DefaultCropRadius;
            displayed = -1;
            retryFrameAt = 0;
            nextWorldCropRequest = 0;
            direction = 0;
            status = null;
            nextPoll = 0;
            EditorApplication.update += Tick;
            Selection.selectionChanged += Repaint;
        }

        private void OnDisable()
        {
            EditorApplication.delayCall -= CloseDuplicate;
            if (owner != this) return;
            owner = null;
            EditorApplication.update -= Tick;
            Selection.selectionChanged -= Repaint;
            if (cursor != null && !restorePaused)
            {
                restorePaused = !cursor.Live;
                restoreFrame = cursor.Frame;
            }
            if (sessionId != 0)
                NativeViewer.Release();
            ClearOverlay();
        }

        private void CloseDuplicate()
        {
            if (this == null) return;
            if (owner != null) owner.Focus();
            Close();
        }

        private void Tick()
        {
            double now = EditorApplication.timeSinceStartup;
            if (now >= nextPoll)
            {
                nextPoll = now + 0.25;
                try
                {
                    status = NativeViewer.Command("status").status;
                    if (status == null) throw new InvalidOperationException("监听状态为空。");
                    var sessions = status.sessions ?? Array.Empty<ViewerSession>();
                    var selected = Array.Find(sessions, s => s.id == sessionId);
                    if (selected == null && sessionId != 0) Select(0);
                    var newest = Array.FindLast(sessions, s => s.frames > 0);
                    if (!restorePaused)
                    {
                        long next = cursor.ChooseSession(sessionId, newest?.id ?? 0, autoSelect);
                        if (next != sessionId)
                        {
                            Select(next);
                            selected = newest;
                        }
                    }
                    ApplySession(selected);
                    if (restorePaused && cursor.Count > 0)
                    {
                        cursor.Seek(restoreFrame);
                        restorePaused = false;
                    }
                    pollError = null;
                }
                catch (Exception e)
                {
                    pollError = Describe(e);
                    nextPoll = now + 1;
                }
                Repaint();
            }

            if (selectedActive && cursor.Live && now >= nextLivePoll)
            {
                nextLivePoll = now + 0.05;
                try
                {
                    long count = NativeViewer.pvd_frame_count((ulong)sessionId);
                    if (count > cursor.Count)
                        cursor.UpdateCount(count);
                }
                catch (Exception e)
                {
                    pollError = Describe(e);
                    nextLivePoll = now + 1;
                }
            }

            if (direction != 0 && now >= nextStep)
            {
                nextStep = now + 1.0 / playbackRate;
                long previous = cursor.Frame;
                cursor.Step(direction);
                if (previous == cursor.Frame) direction = 0;
                Repaint();
            }

            TickFrame(now);
        }

        private void TickFrame(double now)
        {
            var desiredCrop = CurrentCrop(out var unavailable);
            if (cropUnavailable != unavailable)
            {
                cropUnavailable = unavailable;
                retryFrameAt = 0;
                nextWorldCropRequest = 0;
                frameError = null;
                if (unavailable != null)
                {
                    NativeViewer.InvalidateFrame();
                    if (displayed >= 0 && !displayedCrop.Enabled) ClearOverlay();
                    else ClearQueryOverlays();
                }
                Repaint();
            }

            var completed = NativeViewer.TakeFrame();
            if (!desiredCrop.HasValue) return;
            var crop = desiredCrop.Value;
            if (completed != null)
            {
                using var repaints = SceneOverlay.BatchRepaints();
                string previousError = frameError;
                string previousRayError = rayError;
                string previousQueryError = queryError;
                bool raysPreviouslyTruncated = raysTruncated;
                bool queriesPreviouslyTruncated = queriesTruncated;
                bool surfacesDisplayed = false;
                try
                {
                    if (completed.Error != null) throw completed.Error;
                    SceneOverlay.Show(completed.Frame.Records);
                    displayed = completed.Index;
                    displayedCrop = completed.Crop;
                    displayedWithPhysicsQueries = completed.IncludesRays || completed.IncludesQueries;
                    frameError = null;
                    retryFrameAt = 0;
                    surfacesDisplayed = true;
                }
                catch (Exception e)
                {
                    frameError = Describe(e);
                    bool missingQueryEntryPoint = (completed.IncludesRays || completed.IncludesQueries) &&
                        completed.Error is EntryPointNotFoundException;
                    if (!missingQueryEntryPoint && (completed.Error == null || !cropEnabled || !displayedCrop.Enabled))
                        ClearOverlay();
                    else ClearQueryOverlays();
                    if (!cropEnabled) direction = 0;
                    // A paused crop can retry after an input change or an explicit playback action.
                    retryFrameAt = cropEnabled && !cursor.Live && direction == 0 ? double.PositiveInfinity : now + 1;
                }
                if (surfacesDisplayed && completed.IncludesRays) DisplayRays(completed.Frame);
                if (surfacesDisplayed && completed.IncludesQueries) DisplayQueries(completed.Frame, completed.QueryError);
                if (previousError != frameError || previousRayError != rayError || previousQueryError != queryError ||
                    raysPreviouslyTruncated != raysTruncated || queriesPreviouslyTruncated != queriesTruncated)
                    Repaint();
            }

            if (sessionId == 0 || cursor.Frame < 0) return;
            if (cursor.Frame == displayed && displayedCrop.Equals(crop) &&
                displayedWithPhysicsQueries == showPhysicsQueries && !SceneOverlay.NeedsFrame)
            {
                if (crop.Enabled && frameError != null)
                {
                    frameError = null;
                    retryFrameAt = 0;
                    Repaint();
                }
                return;
            }
            if (now < retryFrameAt && (!crop.Enabled || (cursor.Frame == requestedFrame && requestedCrop.Equals(crop)))) return;
            if (crop.Mode == FrameCrop.WorldMode && now < nextWorldCropRequest) return;
            if (NativeViewer.RequestFrame(sessionId, cursor.Frame, room, crop,
                 includeRays: showPhysicsQueries, includeQueries: showPhysicsQueries, latestOnly: selectedLiveOnly))
            {
                requestedFrame = cursor.Frame;
                requestedCrop = crop;
                nextWorldCropRequest = now + 0.05;
            }
        }

        private FrameCrop? CurrentCrop(out string unavailable)
        {
            unavailable = null;
            if (!cropEnabled) return default(FrameCrop);
            if (cropSession != 0)
            {
                if (cropSession == sessionId) return FrameCrop.Actor(cropActor, cropActorGeneration, cropRadius);
                unavailable = "裁切中心属于另一个会话。请切回该会话或选择新的中心。";
            }
            else if (cropTransform == null)
                unavailable = "裁切中心已离开当前场景。请选择新的中心或关闭裁切。";
            else
            {
                var position = cropTransform.position;
                if (FrameCrop.TryWorld(position.x, position.y, position.z, mirrorX, cropRadius, out var crop)) return crop;
                unavailable = "裁切中心没有有效的世界坐标。";
            }
            return null;
        }

        private static string Describe(Exception e)
        {
            if (e is EntryPointNotFoundException)
                return "原生插件接口与编辑器包不匹配。请确认包版本并重启 Unity 编辑器。 " + e.Message;
            if (e is DllNotFoundException || e is BadImageFormatException)
                return "请检查包内的 Windows x86_64 原生插件与编辑器包版本。 " + e.Message;
            return e.Message;
        }

        private void Run(Action action)
        {
            try
            {
                action();
                commandError = null;
                nextPoll = 0;
            }
            catch (Exception e) { commandError = Describe(e); }
        }

        private void Select(long id)
        {
            if (sessionId != 0) NativeViewer.Release();
            sessionId = id;
            selectedActive = false;
            selectedLiveOnly = false;
            room = 0;
            direction = 0;
            restorePaused = false;
            cursor = new PlaybackCursor();
            InvalidateOverlay();
        }

        private void ApplySession(ViewerSession selected)
        {
            selectedActive = selected?.active == true;
            selectedLiveOnly = selected?.liveOnly == true;
            cursor.UpdateCount(selected?.frames ?? 0);
            if (!selectedLiveOnly) return;
            restorePaused = false;
            if (room != 0 && (selected.rooms == null || !Array.Exists(selected.rooms, candidate => candidate.number == room)))
            {
                room = 0;
                InvalidateOverlay();
            }
        }

        private void InvalidateOverlay()
        {
            NativeViewer.ResetFrameReader();
            ClearOverlay();
            retryFrameAt = 0;
            nextWorldCropRequest = 0;
            frameError = null;
        }

        private void ClearOverlay()
        {
            SceneOverlay.Clear();
            displayed = -1;
            displayedWithPhysicsQueries = false;
            raysTruncated = false;
            queriesTruncated = false;
            rayError = null;
            queryError = null;
        }

        private void DisplayRays(SurfaceFrameSnapshot frame)
        {
            try
            {
                SceneOverlay.ShowRays(frame.Rays);
                raysTruncated = frame.RaysTruncated;
                rayError = null;
            }
            catch (Exception e)
            {
                raysTruncated = false;
                rayError = "无法显示物理射线。 " + Describe(e);
                SceneOverlay.ShowRays(null);
            }
        }

        private void DisplayQueries(SurfaceFrameSnapshot frame, Exception unavailable)
        {
            try
            {
                SceneOverlay.ShowQueries(unavailable == null ? frame.Queries : null);
                queriesTruncated = frame.QueriesTruncated;
                queryError = unavailable == null ? null : "无法读取形状查询。 " + Describe(unavailable);
            }
            catch (Exception e)
            {
                queriesTruncated = false;
                queryError = "无法显示形状查询。 " + Describe(e);
                SceneOverlay.ShowQueries(null);
            }
        }

        private void ClearQueryOverlays()
        {
            using var repaints = SceneOverlay.BatchRepaints();
            SceneOverlay.ShowRays(showPhysicsQueries ? Array.Empty<RayRecord>() : null);
            SceneOverlay.ShowQueries(showPhysicsQueries ? Array.Empty<QueryLine>() : null);
            displayedWithPhysicsQueries = false;
            raysTruncated = false;
            queriesTruncated = false;
            rayError = null;
            queryError = null;
        }

        private void OnGUI()
        {
            if (owner != this) return;
            scroll = EditorGUILayout.BeginScrollView(scroll);
            bool listening = status != null && status.listening;
            using (new EditorGUI.DisabledScope(listening))
            {
                address = EditorGUILayout.TextField("监听地址", address);
                recordHistory = EditorGUILayout.Toggle("完整录制", recordHistory);
            }
            if (listening) EditorGUILayout.LabelField("正在监听", status.address);
            if (GUILayout.Button(listening ? "停止监听" : "开始监听"))
                Run(() => NativeViewer.Command(listening ? "stop" : "start", address, recordHistory: recordHistory));

            if (status != null)
            {
                EditorGUILayout.Space();
                DrawSelection();
            }
            DrawCrop();
            if (status != null)
            {
                DrawExport();
                EditorGUILayout.Space();
                EditorGUILayout.LabelField("临时缓存", EditorUtility.FormatBytes(status.totalBytes));
                EditorGUILayout.LabelField("可清理", EditorUtility.FormatBytes(status.clearableBytes));
                // A completed session is protected once its first frame has been acquired.
                using (new EditorGUI.DisabledScope(status.clearableBytes <= 0 || (sessionId != 0 && displayed < 0)))
                {
                    if (GUILayout.Button("清理可释放缓存"))
                        Run(() => released = NativeViewer.Command("clear").released);
                }
                EditorGUILayout.LabelField("上次清理释放", EditorUtility.FormatBytes(released));
            }
            string error = commandError ?? pollError ?? frameError ?? rayError ?? queryError;
            if (!string.IsNullOrEmpty(error)) EditorGUILayout.HelpBox(error, MessageType.Error);
            if (!string.IsNullOrEmpty(status?.error) && status.error != error)
                EditorGUILayout.HelpBox(status.error, MessageType.Warning);
            if (frameError != null && !string.IsNullOrEmpty(status?.frameError))
                EditorGUILayout.HelpBox(status.frameError, MessageType.Error);
            EditorGUILayout.EndScrollView();
        }

        private void DrawSelection()
        {
            var sessions = status.sessions ?? Array.Empty<ViewerSession>();
            var names = new string[sessions.Length + 1];
            names[0] = "关闭视图";
            int current = 0;
            for (int i = 0; i < sessions.Length; i++)
            {
                var s = sessions[i];
                string mode = s.liveOnly ? "仅实时" : "完整录制";
                names[i + 1] = $"会话 {s.id} · {mode} · {(s.active ? "接收中" : "已结束")} · {s.frames} 帧";
                if (s.id == sessionId) current = i + 1;
            }
            int choice = EditorGUILayout.Popup("会话", current, names);
            if (choice != current)
            {
                Run(() =>
                {
                    var chosen = choice == 0 ? null : sessions[choice - 1];
                    Select(chosen?.id ?? 0);
                    ApplySession(chosen);
                    if (chosen != null && !selectedActive && !selectedLiveOnly) cursor.Pause();
                    autoSelect = selectedLiveOnly;
                });
                return;
            }
            if (current == 0) return;

            var rooms = sessions[current - 1].rooms ?? Array.Empty<ViewerRoom>();
            var labels = new string[rooms.Length + 1];
            labels[0] = "全部物理场景";
            int roomIndex = 0;
            for (int i = 0; i < rooms.Length; i++)
            {
                labels[i + 1] = "物理场景 " + rooms[i].number;
                if (rooms[i].number == room) roomIndex = i + 1;
            }
            int roomChoice = EditorGUILayout.Popup("显示范围", roomIndex, labels);
            if (roomChoice != roomIndex)
            {
                room = roomChoice == 0 ? 0 : rooms[roomChoice - 1].number;
                InvalidateOverlay();
            }
            bool showWireframe = EditorGUILayout.Popup("显示模式", wireframe ? 1 : 0, DisplayModes) == 1;
            if (showWireframe != wireframe)
                Run(() => { SceneOverlay.Wireframe = showWireframe; wireframe = showWireframe; });
            showInGameView = EditorGUILayout.Toggle("在 Game 视图显示", showInGameView);
            SceneOverlay.ShowInGameView = showInGameView;
            bool drawQueries = EditorGUILayout.Toggle("显示物理查询", showPhysicsQueries);
            if (drawQueries != showPhysicsQueries)
            {
                showPhysicsQueries = drawQueries;
                RefreshFrame();
            }
            if (raysTruncated)
                EditorGUILayout.HelpBox("当前帧的物理射线可能未完整显示。", MessageType.Info);
            if (queriesTruncated)
                EditorGUILayout.HelpBox("当前帧的形状查询可能未完整显示。", MessageType.Info);
            bool useMirror = EditorGUILayout.Toggle("镜像 X", mirrorX);
            if (useMirror != mirrorX)
            {
                mirrorX = useMirror;
                SceneOverlay.MirrorX = mirrorX;
                if (cropEnabled && cropSession == 0) RefreshFrame();
            }
            if (cursor.Count > 0 && !selectedLiveOnly) DrawTimeline();
            if (GUILayout.Button("关闭当前视图"))
                Run(() => { Select(0); autoSelect = false; });
        }

        private void DrawCrop()
        {
            EditorGUILayout.Space();
            var selected = Selection.activeGameObject;
            using (new EditorGUI.DisabledScope(selected == null || EditorUtility.IsPersistent(selected) || SceneOverlay.IsContainer(selected)))
                if (GUILayout.Button("设为裁切中心")) SetCropCenter(selected);

            EditorGUI.BeginChangeCheck();
            float radius = EditorGUILayout.DelayedFloatField("裁切半径", cropRadius);
            if (EditorGUI.EndChangeCheck())
            {
                if (FrameCrop.IsRadiusValid(radius))
                {
                    cropRadius = radius;
                    commandError = null;
                    if (cropEnabled) RefreshFrame();
                }
                else commandError = "裁切半径必须是有限正数。";
            }

            if (!cropEnabled) return;
            EditorGUILayout.LabelField("裁切中心", cropTransform != null ? cropTransform.name : cropLabel);
            if (!string.IsNullOrEmpty(cropUnavailable))
                EditorGUILayout.HelpBox(cropUnavailable, MessageType.Warning);
            if ((cropUnavailable != null || frameError != null) && displayed >= 0 && displayedCrop.Enabled)
                EditorGUILayout.LabelField("裁切画面", "保留上次成功的画面");
            if (GUILayout.Button("关闭裁切"))
            {
                cropEnabled = false;
                cropTransform = null;
                cropSession = 0;
                cropLabel = null;
                commandError = null;
                RefreshFrame();
            }
        }

        private void SetCropCenter(GameObject selected)
        {
            cropEnabled = true;
            if (SceneOverlay.TryGetActor(selected, out cropActor, out cropActorGeneration))
            {
                cropSession = sessionId;
                cropTransform = null;
                cropLabel = $"刚体 {cropActor}（代际 {cropActorGeneration}）";
            }
            else
            {
                cropSession = 0;
                cropTransform = selected.transform;
                cropLabel = selected.name;
            }
            commandError = null;
            RefreshFrame();
        }

        private void DrawExport()
        {
            var export = status.export;
            using (new EditorGUI.DisabledScope(sessionId == 0 || selectedLiveOnly || cursor.Count == 0 || export?.running == true))
            {
                if (GUILayout.Button("导出选中会话（.pvdcapture）"))
                {
                    long id = sessionId;
                    string path = EditorUtility.SaveFilePanel("导出选中会话的全部可回放帧", "", $"session-{id}", "pvdcapture");
                    if (!string.IsNullOrEmpty(path))
                        Run(() => NativeViewer.Command("export", session: id, path: path));
                }
            }
            if (export == null || export.session == 0) return;
            if (export.running)
                EditorGUILayout.LabelField($"正在导出会话 {export.session}", export.path);
            else if (!string.IsNullOrEmpty(export.error))
                EditorGUILayout.HelpBox($"会话 {export.session} 导出失败\n{export.error}", MessageType.Error);
            else
                EditorGUILayout.HelpBox($"已导出会话 {export.session} · {export.frames} 帧 · {EditorUtility.FormatBytes(export.bytes)}\n{export.path}", MessageType.Info);
        }

        private void DrawTimeline()
        {
            long last = cursor.Count - 1;
            EditorGUILayout.LabelField("回放", $"{cursor.Frame + 1} / {cursor.Count}  {(cursor.Live ? "实时" : direction == 0 ? "已暂停" : "播放中")}");
            using (new EditorGUI.DisabledScope(last == 0))
            {
                EditorGUI.BeginChangeCheck();
                float position = GUILayout.HorizontalSlider(last > 0 ? (float)((double)cursor.Frame / last) : 0, 0, 1);
                if (EditorGUI.EndChangeCheck())
                {
                    ChangePlaybackTarget();
                    cursor.Seek((long)Math.Round((double)position * last));
                }
            }
            EditorGUILayout.BeginHorizontal();
            if (GUILayout.Button("上一帧")) { ChangePlaybackTarget(); cursor.Step(-1); }
            if (GUILayout.Button("暂停"))
            {
                ChangePlaybackTarget();
                cursor.Seek(displayed >= 0 ? displayed : cursor.Frame);
            }
            if (GUILayout.Button("下一帧")) { ChangePlaybackTarget(); cursor.Step(1); }
            if (GUILayout.Button("返回实时")) { ChangePlaybackTarget(); cursor.ResumeLive(); }
            EditorGUILayout.EndHorizontal();

            playbackRate = EditorGUILayout.IntSlider("回放帧率", playbackRate, 1, 60);
            EditorGUILayout.BeginHorizontal();
            if (GUILayout.Button("倒放")) BeginPlayback(-1);
            if (GUILayout.Button("正放")) BeginPlayback(1);
            EditorGUILayout.EndHorizontal();
        }

        private void BeginPlayback(int value)
        {
            ChangePlaybackTarget();
            cursor.Pause();
            direction = value;
            nextStep = 0;
        }

        private void ChangePlaybackTarget()
        {
            direction = 0;
            RefreshFrame();
        }

        private void RefreshFrame()
        {
            NativeViewer.InvalidateFrame();
            ClearQueryOverlays();
            retryFrameAt = 0;
            nextWorldCropRequest = 0;
            frameError = null;
        }
    }
}
