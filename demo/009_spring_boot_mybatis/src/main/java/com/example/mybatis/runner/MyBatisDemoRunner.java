package com.example.mybatis.runner;

import com.example.mybatis.model.AnalyticsMetric;
import com.example.mybatis.model.Subscription;
import com.example.mybatis.model.User;
import com.example.mybatis.service.UserService;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.springframework.boot.CommandLineRunner;
import org.springframework.stereotype.Component;

import java.util.List;

@Component
public class MyBatisDemoRunner implements CommandLineRunner {

    private static final Logger log = LoggerFactory.getLogger(MyBatisDemoRunner.class);
    private final UserService userService;

    public MyBatisDemoRunner(UserService userService) {
        this.userService = userService;
    }

    @Override
    public void run(String... args) throws Exception {
        log.info("================================================================================");
        log.info("  🚀 H2 Database Rust - Spring Boot 4.1 / MyBatis (XML) Comprehensive Demo");
        log.info("================================================================================");

        try {
            // 1. スキーマとシーケンスの初期化
            log.info("\n[1] Initializing schema, sequence, and tables via MyBatis XML Mappers...");
            userService.initializeSchema();
            log.info("  ✓ Created sequence 'user_id_seq'");
            log.info("  ✓ Created tables 'users', 'metrics', 'subscriptions'");

            // 2. Primary への書き込みと SEQUENCE 採番の実演
            log.info("\n[2] Registering users to Primary node (Read-Write) via SEQUENCE NEXTVAL...");
            User u1 = userService.registerUser("alice_tokyo", "alice@example.com");
            User u2 = userService.registerUser("bob_osaka", "bob@example.com");
            User u3 = userService.registerUser("carol_kyoto", "carol@example.com");
            log.info("  ✓ Inserted User 1: {}", u1);
            log.info("  ✓ Inserted User 2: {}", u2);
            log.info("  ✓ Inserted User 3: {}", u3);

            // 3. トランザクション・ロールバックの実演
            log.info("\n[3] Testing Spring Transaction Rollback on Primary node...");
            try {
                userService.registerUserAndRollback("dave_temp", "dave@example.com");
            } catch (IllegalStateException e) {
                log.info("  ✓ Caught expected exception: {}", e.getMessage());
                log.info("  ✓ Transaction rollback verified!");
            }

            // 4. Standby (Read-Only) レプリカからのデータ読み込み (@Transactional(readOnly = true))
            log.info("\n[4] Querying from Standby node via @Transactional(readOnly = true) routing...");
            List<User> usersFromStandby = userService.getAllUsersFromStandby();
            log.info("  ✓ Retrieved {} users from Standby:", usersFromStandby.size());
            for (User u : usersFromStandby) {
                log.info("    -> {}", u);
            }

            // 5. MyBatis ResultHandler によるサーバサイドカーソル風走査
            log.info("\n[5] Streaming fetch using MyBatis ResultHandler / Cursor...");
            List<User> streamed = userService.streamUsersWithCursor();
            log.info("  ✓ Streamed {} users successfully.", streamed.size());

            // 6. 高度な数学・文字列関数 (LN, EXP, TRUNC, INITCAP, REGEXP_REPLACE)
            log.info("\n[6] Computing advanced Math & String functions in SQL...");
            userService.recordMetric("cpu_01_core", 2.7182818);
            userService.recordMetric("cpu_02_aux", 10.5543);
            userService.recordMetric("mem_buffer", 100.0);

            List<AnalyticsMetric> metrics = userService.getCalculatedMetrics();
            for (AnalyticsMetric m : metrics) {
                log.info("    Metric: {} | Raw: {} | LN: {} | EXP: {} | TRUNC: {} | INITCAP: {} | REPLACED: {}",
                        m.getMetricName(), m.getRawVal(), m.getLnVal(), m.getExpVal(), m.getTruncVal(),
                        m.getInitcapStr(), m.getReplacedStr());
            }

            // 7. 正規表現演算子 (~) によるクエリ検索
            log.info("\n[7] Querying with POSIX Regular Expression Operator (~)...");
            List<AnalyticsMetric> matched = userService.searchMetricsByRegex("^cpu_[0-9]+");
            log.info("  ✓ Matched {} metrics matching '^cpu_[0-9]+':", matched.size());
            for (AnalyticsMetric m : matched) {
                log.info("    -> {}", m.getMetricName());
            }

            // 8. SERIAL 型と INTERVAL 型演算の実演
            log.info("\n[8] Testing SERIAL auto-increment and INTERVAL date arithmetic...");
            userService.addSubscription(u1.getId(), "PRO_ANNUAL");
            userService.addSubscription(u2.getId(), "ENTERPRISE_MONTHLY");

            List<Subscription> subs = userService.getSubscriptions();
            log.info("  ✓ Retrieved {} subscriptions with computed expiration intervals:", subs.size());
            for (Subscription s : subs) {
                log.info("    Subscription ID: {} | User: {} | Plan: {} | Start: {} | Expire (+30 days): {}",
                        s.getId(), s.getUserId(), s.getPlanName(), s.getStartTime(), s.getExpireTime());
            }

            log.info("\n================================================================================");
            log.info("  ✨ All Spring Boot 4.1 / MyBatis (XML) demonstrations finished successfully!");
            log.info("================================================================================\n");

        } catch (Exception e) {
            log.error("Demo failed with exception: ", e);
            throw e;
        }
    }
}
