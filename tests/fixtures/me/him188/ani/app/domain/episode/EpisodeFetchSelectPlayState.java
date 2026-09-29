package me.him188.ani.app.domain.episode;

import java.util.List;
import org.openani.mediamp.features.PlayerFeatures;
import java.awt.EventQueue;
import kotlin.coroutines.Continuation;
import kotlinx.coroutines.flow.StateFlow;

public class EpisodeFetchSelectPlayState {
    private static volatile boolean fallbackNames;
    private static volatile boolean failSubject;
    private static volatile Runnable duringRead;
    private final Flow sessions = new Flow(new Session(1));
    private final PlayerSession playerSession = new PlayerSession();
    public Flow getEpisodeSessionFlow() { return sessions; }
    public PlayerSession getPlayerSession() { return playerSession; }
    public void onUIReady() { System.out.println("ready"); }
    public Object onClose(Continuation continuation) { return null; }

    static class Flow implements StateFlow {
        public volatile Object value;
        Flow(Object value) { this.value = value; }
        public Object getValue() { return value; }
        public List<Object> getReplayCache() { return value == null ? List.of() : List.of(value); }
    }
    public static class Session {
        private final int id;
        private final Flow info;
        Session(int id) { this.id = id; this.info = new Flow(new Info(id)); }
        public int getEpisodeId() { return id; }
        public Flow getInfoBundleFlow() { return info; }
    }
    public static class Info {
        private final int id;
        Info(int id) { this.id = id; }
        public int getSubjectId() { return 42; }
        public int getEpisodeId() { return id; }
        public Subject getSubjectInfo() {
            if (failSubject) throw new IllegalStateException("fixture getter failure");
            var hook = duringRead;
            if (hook != null) hook.run();
            return new Subject();
        }
        public Episode getEpisodeInfo() { return new Episode(id); }
    }
    public static class Subject {
        public String getNameCn() { return fallbackNames ? " \t" : "测试番剧"; }
        public String getName() { return "Test anime"; }
        public String getImageLarge() { return "https://example.com/cover.jpg"; }
    }
    public static class Episode {
        private final int id;
        Episode(int id) { this.id = id; }
        public String getNameCn() { return fallbackNames ? "" : "第" + id + "集"; }
        public String getName() { return "Episode"; }
        public String getSort() { return String.valueOf(id); }
    }
    public static class PlayerSession {
        private final Player player = new Player();
        public Player getPlayer() { return player; }
    }
    public static class Player {
        public final PlayerFeatures features = new PlayerFeatures();
        public PlayerFeatures getFeatures() { return features; }
        public final Flow state = new Flow(new State("Ready", true, false));
        public final Flow position = new Flow(12000L);
        public final Flow properties = new Flow(new Properties(1440000L));
        public Flow getState() { return state; }
        public StateFlow getCurrentPositionMillis() { return position; }
        public Flow getMediaProperties() { return properties; }
        public void play() {
            if (!EventQueue.isDispatchThread()) throw new IllegalStateException("play called off UI thread");
            state.value = new State("Ready", true, false);
            System.out.println("transport:play");
        }
        public void pause() {
            if (!EventQueue.isDispatchThread()) throw new IllegalStateException("pause called off UI thread");
            state.value = new State("Ready", false, false);
            System.out.println("transport:pause");
        }
    }
    public static class State {
        private final String status;
        private final boolean playing, buffering;
        State(String status, boolean playing, boolean buffering) { this.status = status; this.playing = playing; this.buffering = buffering; }
        public String getMediaStatus() { return status; }
        public boolean getPlayWhenReady() { return playing; }
        public boolean isPlaying() { return playing; }
        public boolean isBuffering() { return buffering; }
    }
    public static class Properties {
        private final Long duration;
        Properties(Long duration) { this.duration = duration; }
        public Long getDurationMillis() { return duration; }
    }

    public static void main(String[] args) throws Exception {
        if (args.length > 0 && args[0].equals("speed")) {
            speedCases();
            return;
        }
        if (args.length > 0 && args[0].equals("edges")) {
            edgeCases();
            return;
        }
        var owner = new EpisodeFetchSelectPlayState();
        owner.onUIReady();
        awaitSample();
        owner.playerSession.player.state.value = new State("Ready", false, false);
        owner.playerSession.player.position.value = 45000L;
        awaitSample();
        owner.playerSession.player.state.value = new State("Ready", false, true);
        awaitSample();
        owner.playerSession.player.state.value = new State("Opening", false, false);
        awaitSample();
        owner.sessions.value = new Session(2);
        owner.playerSession.player.position.value = 1000L;
        owner.playerSession.player.state.value = new State("Ready", true, false);
        awaitSample();
        owner.onClose(null);
        awaitSample();
    }

    private static void speedCases() throws Exception {
        var owner = new EpisodeFetchSelectPlayState();
        var player = owner.playerSession.player;
        owner.onUIReady();
        awaitSample();
        player.features.rate = 2.0f;
        awaitSample();
        player.state.value = new State("Ready", false, false);
        awaitSample();
        player.state.value = new State("Ready", true, true);
        awaitSample();
        player.state.value = new State("Ready", false, true);
        awaitSample();
        player.features.hasSpeed = false;
        awaitSample();
        player.features.hasSpeed = true;
        player.features.rate = Float.NaN;
        awaitSample();
        player.features.rate = 1.0f;
        awaitSample();
        owner.onClose(null);
        awaitSample();
    }

    private static void edgeCases() throws Exception {
        var owner = new EpisodeFetchSelectPlayState();
        var player = owner.playerSession.player;
        owner.onUIReady();
        awaitSample();
        player.state.value = new State("Ended", true, true);
        awaitSample();
        player.state.value = new State("Ready", true, true);
        awaitSample();
        player.state.value = new State("Ready", true, false);
        fallbackNames = true;
        player.properties.value = new Properties(null);
        awaitSample();
        player.properties.value = null;
        awaitSample();
        ((Session) owner.sessions.value).info.value = null;
        awaitSample();
        owner.sessions.value = new Session(1);
        awaitSample();
        ((Session) owner.sessions.value).info.value = new Info(9);
        awaitSample();
        owner.sessions.value = new Session(1);
        awaitSample();
        failSubject = true;
        awaitSample();
        failSubject = false;
        awaitSample();
        duringRead = () -> owner.sessions.value = new Session(1);
        awaitSample();
        duringRead = null;
        awaitSample();
        duringRead = () -> player.state.value = new State("Ready", true, false);
        awaitSample();
        duringRead = null;
        awaitSample();
        var nextOwner = new EpisodeFetchSelectPlayState();
        nextOwner.sessions.value = new Session(2);
        nextOwner.onUIReady();
        owner.onClose(null);
        awaitSample();
        nextOwner.onUIReady();
        awaitSample();
        nextOwner.onClose(null);
        awaitSample();
    }

    private static void awaitSample() throws Exception {
        if (System.in.read() < 0) throw new IllegalStateException("Test controller disconnected");
    }
}
