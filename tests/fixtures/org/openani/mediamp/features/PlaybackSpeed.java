package org.openani.mediamp.features;

public interface PlaybackSpeed extends Feature {
    Key Key = new Key();
    final class Key implements FeatureKey {}
    float getValue();
}
