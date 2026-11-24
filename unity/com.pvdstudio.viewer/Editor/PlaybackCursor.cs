using System;
using System.Runtime.CompilerServices;

[assembly: InternalsVisibleTo("PvdStudio.Editor.Tests")]

namespace PvdStudio.Editor
{
    internal sealed class PlaybackCursor
    {
        internal long Frame { get; private set; }
        internal bool Live { get; private set; }
        internal long Count { get; private set; }

        internal PlaybackCursor()
        {
            ResumeLive();
        }

        internal void UpdateCount(long count)
        {
            Count = Math.Max(count, 0);
            Frame = Live ? Count - 1 : Clamp(Frame);
        }

        internal void Pause() { Live = false; }

        internal long ChooseSession(long current, long newestReady, bool openAutomatically)
        {
            if (!Live || (current == 0 && !openAutomatically)) return current;
            return Math.Max(current, newestReady);
        }

        internal void ResumeLive()
        {
            Live = true;
            Frame = Count - 1;
        }

        internal void Seek(long index)
        {
            Pause();
            Frame = Clamp(index);
        }

        internal void Step(int direction)
        {
            Seek(Frame + Math.Sign(direction));
        }

        private long Clamp(long index)
        {
            return Math.Min(Math.Max(0L, index), Count - 1);
        }
    }
}
