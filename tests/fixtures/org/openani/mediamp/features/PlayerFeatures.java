package org.openani.mediamp.features;

public class PlayerFeatures {
    public volatile float rate = 1.0f;
    public volatile boolean hasSpeed = true;
    private final PlaybackSpeed speed = () -> rate;
    public Feature get(FeatureKey key) { return key == PlaybackSpeed.Key && hasSpeed ? speed : null; }
}
