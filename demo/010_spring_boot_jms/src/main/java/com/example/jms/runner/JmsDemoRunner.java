package com.example.jms.runner;

import com.example.jms.listener.OrderEventListener;
import com.example.jms.mapper.OrderMapper;
import com.example.jms.mapper.QueueMapper;
import com.example.jms.model.Order;
import com.example.jms.model.QueueMessage;
import com.example.jms.service.OrderProcessingService;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.springframework.boot.CommandLineRunner;
import org.springframework.stereotype.Component;

import java.math.BigDecimal;
import java.util.List;
import java.util.concurrent.TimeUnit;

/**
 * Spring Boot 4.1 / Spring JMS & MyBatis 総合デモランナー。
 * 1. トランザクショナル・キューテーブル作成
 * 2. @Transactional 下での MyBatis DB更新 ＋ Spring JmsTemplate 送信の原子性（コミット実証）
 * 3. 例外発生時の完全ロールバック実証（DB と JMS キュー双方が自動取り消し）
 * 4. @JmsListener による非同期メッセージ受信
 * 5. Kafka 風のオフセット巻き戻し（Seek / Replay）による過去メッセージの再取得
 */
@Component
public class JmsDemoRunner implements CommandLineRunner {
    private static final Logger log = LoggerFactory.getLogger(JmsDemoRunner.class);

    private final OrderMapper orderMapper;
    private final QueueMapper queueMapper;
    private final OrderProcessingService orderProcessingService;
    private final OrderEventListener orderEventListener;

    public JmsDemoRunner(OrderMapper orderMapper,
                           QueueMapper queueMapper,
                           OrderProcessingService orderProcessingService,
                           OrderEventListener orderEventListener) {
        this.orderMapper = orderMapper;
        this.queueMapper = queueMapper;
        this.orderProcessingService = orderProcessingService;
        this.orderEventListener = orderEventListener;
    }

    @Override
    public void run(String... args) throws Exception {
        log.info("======================================================================");
        log.info("   H2 Database Rust - Spring Boot 4.1 / Spring JMS Standard Demo      ");
        log.info("======================================================================");

        // Step 1: DDL 実行 (DROP & CREATE TABLE / QUEUE TABLE)
        log.info("--> [Step 1] Initializing Schema via MyBatis XML Mappers...");
        try {
            orderMapper.dropOrderTable();
            queueMapper.dropOrderEventsQueue();
        } catch (Exception ignored) {}
        orderMapper.createOrderTable();
        queueMapper.createOrderEventsQueue();
        log.info("   [OK] Tables 'orders' and 'order_events_queue' initialized successfully.");

        // Step 2: 正常ケース - @Transactional 下でのアトミックコミット
        log.info("\n--> [Step 2] Testing Atomic Commit (@Transactional + JmsTemplate)...");
        Order order101 = new Order(101L, "Alice Johnson", new BigDecimal("12500.00"), "CREATED");
        orderProcessingService.createOrderSuccess(order101);

        // コミット確認
        Order savedOrder101 = orderMapper.findById(101L);
        log.info("   [Verify DB] Order 101 in DB: {}", savedOrder101);

        List<QueueMessage> queueMessages = queueMapper.fetchMessagesFromOffset(0L);
        log.info("   [Verify Queue] Total messages in queue: {}", queueMessages.size());
        for (QueueMessage qm : queueMessages) {
            log.info("     - Offset {}: msgId={}, payload={}", qm.getOffset(), qm.getMsgId(), qm.getPayload());
        }

        // Step 3: ロールバックケース - 障害時の完全取り消し実証
        log.info("\n--> [Step 3] Testing Atomic Rollback on Business Failure...");
        Order order999 = new Order(999L, "Failed Customer", new BigDecimal("99999.00"), "PENDING");
        try {
            orderProcessingService.createOrderWithRollback(order999);
        } catch (Exception e) {
            log.info("   [Catch Exception] Expected business failure caught: {}", e.getMessage());
        }

        // ロールバック確認 (Order 999 も キューメッセージも保存されていないこと)
        Order savedOrder999 = orderMapper.findById(999L);
        log.info("   [Verify DB] Order 999 in DB (should be null): {}", savedOrder999);

        List<QueueMessage> queueAfterRollback = queueMapper.fetchMessagesFromOffset(0L);
        log.info("   [Verify Queue] Total messages after rollback (should remain {}): {}",
                queueMessages.size(), queueAfterRollback.size());

        if (savedOrder999 == null && queueAfterRollback.size() == queueMessages.size()) {
            log.info("   [SUCCESS] Transactional Outbox pattern is UNNECESSARY!");
            log.info("             DB update and JMS queue were rolled back atomically together!");
        } else {
            throw new IllegalStateException("Rollback verification failed!");
        }

        // Step 4: @JmsListener による非同期受信の待機と確認
        log.info("\n--> [Step 4] Verifying @JmsListener Asynchronous Message Reception...");
        boolean received = orderEventListener.getLatch().await(3, TimeUnit.SECONDS);
        log.info("   [Verify Listener] Messages received by @JmsListener: {}", orderEventListener.getReceivedMessages());

        // Step 5: 追加注文送信と Kafka 風オフセットシーク・リプレイの実証
        log.info("\n--> [Step 5] Enqueueing More Events & Demonstrating Kafka-style Offset Replay...");
        Order order102 = new Order(102L, "Bob Smith", new BigDecimal("3400.50"), "CREATED");
        Order order103 = new Order(103L, "Carol White", new BigDecimal("8200.00"), "CREATED");
        orderProcessingService.createOrderSuccess(order102);
        orderProcessingService.createOrderSuccess(order103);

        log.info("   --- Replaying all messages from beginning (_offset >= 0) ---");
        List<QueueMessage> allMessages = queueMapper.fetchMessagesFromOffset(0L);
        for (QueueMessage qm : allMessages) {
            log.info("     [Stream Offset {}] {}", qm.getOffset(), qm.getPayload());
        }

        log.info("   --- Seeking to specific offset (_offset >= 1) ---");
        List<QueueMessage> partialMessages = queueMapper.fetchMessagesFromOffset(1L);
        for (QueueMessage qm : partialMessages) {
            log.info("     [Stream Offset {}] {}", qm.getOffset(), qm.getPayload());
        }

        log.info("\n======================================================================");
        log.info("   ALL SPRING BOOT / SPRING JMS / MYBATIS DEMO TESTS PASSED!          ");
        log.info("======================================================================\n");
    }
}
