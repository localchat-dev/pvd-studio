using NUnit.Framework;

namespace PvdStudio.Editor.Tests
{
    public class PlaybackCursorTests
    {
        [Test]
        public void PauseRetainsDisplayedFrameAsHistoryGrows()
        {
            var cursor = new PlaybackCursor();
            cursor.UpdateCount(4);
            Assert.AreEqual(3L, cursor.Frame);
            cursor.Pause();
            cursor.UpdateCount(8);
            Assert.AreEqual(3L, cursor.Frame);
            cursor.ResumeLive();
            Assert.AreEqual(7L, cursor.Frame);
        }

        [Test]
        public void SteppingAndSeekingStayWithinPublishedFrames()
        {
            var cursor = new PlaybackCursor();
            cursor.UpdateCount(0);
            Assert.AreEqual(-1L, cursor.Frame);
            cursor.UpdateCount(3);
            cursor.Step(-1);
            Assert.AreEqual(1L, cursor.Frame);
            cursor.Seek(long.MaxValue);
            cursor.Step(1);
            Assert.AreEqual(2L, cursor.Frame);
            cursor.Seek(long.MinValue);
            cursor.Step(-1);
            Assert.AreEqual(0L, cursor.Frame);
            Assert.AreEqual(false, cursor.Live);
        }

        [Test]
        public void ReconnectFollowsOnlyInLiveModeAndRespectsClosedView()
        {
            var cursor = new PlaybackCursor();
            Assert.AreEqual(3L, cursor.ChooseSession(2, 3, true));
            Assert.AreEqual(2L, cursor.ChooseSession(2, 0, true));
            cursor.Pause();
            Assert.AreEqual(2L, cursor.ChooseSession(2, 3, true));
            cursor.ResumeLive();
            Assert.AreEqual(0L, cursor.ChooseSession(0, 3, false));
            Assert.AreEqual(3L, cursor.ChooseSession(0, 3, true));
        }
    }
}
