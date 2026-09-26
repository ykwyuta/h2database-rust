package com.example.graph.config;

import org.neo4j.driver.AuthTokens;
import org.neo4j.driver.Config;
import org.neo4j.driver.Driver;
import org.neo4j.driver.GraphDatabase;
import org.springframework.beans.factory.annotation.Value;
import org.springframework.context.annotation.Bean;
import org.springframework.context.annotation.Configuration;

import java.util.concurrent.TimeUnit;

/**
 * Interface 1 (Bolt / Neo4j Java Driver) の接続設定クラス。
 * H2 Database Rust の組み込み Bolt サーバーへ接続する Driver Bean を提供します。
 */
@Configuration
public class BoltDriverConfig {

    @Value("${h2.graph.bolt-uri:bolt://localhost:7687}")
    private String boltUri;

    @Bean(destroyMethod = "close")
    public Driver neo4jDriver() {
        Config config = Config.builder()
                .withoutEncryption()
                .withConnectionTimeout(5, TimeUnit.SECONDS)
                .build();
        return GraphDatabase.driver(boltUri, AuthTokens.none(), config);
    }
}
