using System;
using System.Threading;
using NUnit.Framework;
using UnityEngine;

namespace PvdStudio.Editor.Tests
{
    public class FrameWorkerTests
    {
        [Test]
        public void BusyWorkerSkipsIntermediateTargetsAndPublishesProgress()
        {
            using (var started = new ManualResetEventSlim())
            using (var finish = new ManualResetEventSlim())
            {
                var worker = new FrameWorker();
                int main = Thread.CurrentThread.ManagedThreadId;
                int fetchThread = main;
                worker.Start(1, () => {
                    fetchThread = Thread.CurrentThread.ManagedThreadId;
                    started.Set();
                    if (!finish.Wait(5000)) throw new TimeoutException();
                    return default;
                });
                Assert.IsTrue(started.Wait(5000));
                try
                {
                    Assert.AreNotEqual(main, fetchThread);
                    Assert.IsTrue(!worker.Start(2, () => default));
                    Assert.IsNull(worker.Take());
                }
                finally { finish.Set(); }
                worker.Wait();
                Assert.AreEqual(1L, worker.Take().Index);
                Assert.IsTrue(worker.Start(3, () => default));
                worker.Wait();
                Assert.AreEqual(3L, worker.Take().Index);
            }
        }

        [Test]
        public void InvalidationDiscardsFramesAndErrorsEvenWhenAlreadyCompleted()
        {
            var worker = new FrameWorker();
            worker.Start(4, () => { throw new InvalidOperationException("old room"); });
            worker.Wait();
            worker.Invalidate();
            Assert.IsNull(worker.Take());
            Assert.IsTrue(worker.Start(4, () => { throw new InvalidOperationException("new room"); }));
            worker.Wait();
            Assert.AreEqual("new room", worker.Take().Error.Message);
        }

        [Test]
        public void InvalidatedRayRequestCannotPublishWhenChangingDisplayAtTheSameFrame()
        {
            var worker = new FrameWorker();
            var rays = new[] { new RayRecord(Vector3.zero, new Vector3(0, 1, 0), Vector3.zero, false, false) };
            var view = new SurfaceFrameSnapshot(Array.Empty<SurfaceRecord>(), rays, true);
            Assert.IsTrue(worker.Start(4, () => view, includeRays: true));
            worker.Wait();
            worker.Invalidate();
            Assert.IsNull(worker.Take());

            var noRays = new SurfaceFrameSnapshot(Array.Empty<SurfaceRecord>(), Array.Empty<RayRecord>(), false);
            Assert.IsTrue(worker.Start(4, () => noRays));
            worker.Wait();
            var disabled = worker.Take();
            Assert.AreEqual(4L, disabled.Index);
            Assert.IsFalse(disabled.IncludesRays);
            Assert.AreEqual(0, disabled.Frame.Rays.Length);

            Assert.IsTrue(worker.Start(4, () => view, includeRays: true));
            worker.Wait();
            var enabled = worker.Take();
            Assert.IsTrue(enabled.IncludesRays);
            Assert.IsTrue(enabled.Frame.RaysTruncated);
            Assert.AreSame(rays, enabled.Frame.Rays);
        }

        [Test]
        public void MissingQueryEntryPointStillPublishesFallbackSurfacesAndRays()
        {
            var worker = new FrameWorker();
            var fallback = new SurfaceFrameSnapshot(new[] { new SurfaceRecord() }, Array.Empty<RayRecord>(), false);
            var unavailable = new EntryPointNotFoundException("pvd_frame_queries");
            Assert.IsTrue(worker.Start(4, () => { throw unavailable; }, includeRays: true,
                includeQueries: true, fetchWithoutQueries: () => fallback));
            worker.Wait();
            var completed = worker.Take();
            Assert.AreEqual(4L, completed.Index);
            Assert.IsTrue(completed.IncludesRays);
            Assert.IsTrue(completed.IncludesQueries);
            Assert.AreSame(unavailable, completed.QueryError);
            Assert.IsNull(completed.Error);
            Assert.AreSame(fallback.Records, completed.Frame.Records);

            Assert.IsTrue(worker.Start(4, () => fallback));
            worker.Wait();
            var disabled = worker.Take();
            Assert.IsFalse(disabled.IncludesRays);
            Assert.IsFalse(disabled.IncludesQueries);
            Assert.IsNull(disabled.QueryError);
        }

        [Test]
        public void SurfaceOnlyBridgeStillPublishesFrameWhenBothQueryExportsAreMissing()
        {
            var worker = new FrameWorker();
            var surfaces = new SurfaceFrameSnapshot(new[] { new SurfaceRecord() }, Array.Empty<RayRecord>(), false);
            var missingQueries = new EntryPointNotFoundException("pvd_frame_queries");
            var missingRays = new EntryPointNotFoundException("pvd_frame_view");
            Assert.IsTrue(worker.Start(5, () => { throw missingQueries; }, includeRays: true, includeQueries: true,
                fetchWithoutQueries: () => NativeViewer.FetchWithoutQueries((rays, queries) => {
                    Assert.IsFalse(queries);
                    if (rays) throw missingRays;
                    return surfaces;
                }, includeRays: true)));
            worker.Wait();
            var completed = worker.Take();
            Assert.AreSame(missingQueries, completed.QueryError);
            Assert.IsNull(completed.Error);
            Assert.AreSame(surfaces.Records, completed.Frame.Records);
        }

        [Test]
        public void CropMovementChangesRequestEvenAtTheSameFrame()
        {
            Assert.IsTrue(FrameCrop.TryWorld(10, 2, 3, true, 20, out var previous));
            Assert.IsTrue(FrameCrop.TryWorld(11, 2, 3, true, 20, out var next));
            Assert.AreEqual(-10f, previous.X);
            Assert.AreNotEqual(previous, next);

            var worker = new FrameWorker();
            Assert.IsTrue(worker.Start(4, () => default, previous));
            worker.Wait();
            Assert.IsTrue(!worker.Start(4, () => default, next));
            var completed = worker.Take();
            Assert.AreEqual(4L, completed.Index);
            Assert.AreEqual(previous, completed.Crop);
            Assert.IsTrue(worker.Start(4, () => default, next));
            worker.Wait();
            Assert.AreEqual(next, worker.Take().Crop);
        }

        [Test]
        public void CropInputRequiresFinitePositionAndPositiveFiniteRadius()
        {
            Assert.IsTrue(!FrameCrop.TryWorld(float.NaN, 2, 3, false, 20, out _));
            Assert.IsTrue(!FrameCrop.TryWorld(1, 2, 3, false, float.PositiveInfinity, out _));
            Assert.IsTrue(!FrameCrop.TryWorld(1, 2, 3, false, 0, out _));
            Assert.IsTrue(!FrameCrop.TryWorld(1, 2, 3, false, -1, out _));
            Assert.IsTrue(FrameCrop.TryWorld(1, 2, 3, false, 20, out var crop));
            Assert.AreEqual(1f, crop.X);
            Assert.AreEqual(2f, crop.Y);
            Assert.AreEqual(3f, crop.Z);
            Assert.AreEqual(20f, crop.Radius);
        }

        [Test]
        public void ActorCropIncludesGenerationAndRadius()
        {
            var crop = FrameCrop.Actor(101, 1, 20);
            Assert.AreEqual(crop, FrameCrop.Actor(101, 1, 20));
            Assert.AreNotEqual(crop, FrameCrop.Actor(101, 2, 20));
            Assert.AreNotEqual(crop, FrameCrop.Actor(101, 1, 10));
            Assert.AreNotEqual(crop, default(FrameCrop));
        }
    }
}
