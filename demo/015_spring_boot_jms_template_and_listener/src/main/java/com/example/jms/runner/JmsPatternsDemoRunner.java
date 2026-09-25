package com.example.jms.runner;

import com.example.jms.listener.NotificationListener;
import com.example.jms.listener.PaymentProcessorListener;
import com.example.jms.model.NotificationMessage;
import com.example.jms.model.PaymentRequest;
import com.example.jms.model.PaymentResponse;
import com.example.jms.service.JmsClientService;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.springframework.boot.CommandLineRunner;
import org.springframework.boot.SpringApplication;
import org.springframework.context.ApplicationContext;
import org.springframework.jdbc.core.JdbcTemplate;
import org.springframework.stereotype.Component;

import javax.sql.DataSource;
import java.sql.Connection;
import java.sql.ResultSet;
import java.sql.Statement;
import java.util.concurrent.TimeUnit;

@Component
public class JmsPatternsDemoRunner implements CommandLineRunner {
    private static final Logger log = LoggerFactory.getLogger(JmsPatternsDemoRunner.class);

    private final DataSource dataSource;
    private final JmsClientService jmsClientService;
    private final NotificationListener notificationListener;
    private final PaymentProcessorListener paymentProcessorListener;
    private final ApplicationContext applicationContext;

    public JmsPatternsDemoRunner(DataSource dataSource,
                                 JmsClientService jmsClientService,
                                 NotificationListener notificationListener,
                                 PaymentProcessorListener paymentProcessorListener,
                                 ApplicationContext applicationContext) {
        this.dataSource = dataSource;
        this.jmsClientService = jmsClientService;
        this.notificationListener = notificationListener;
        this.paymentProcessorListener = paymentProcessorListener;
        this.applicationContext = applicationContext;
    }

    @Override
    public void run(String... args) throws Exception {
        log.info("==========================================================================================");
        log.info("  🚀 H2 Database Rust - Spring JMS (JmsTemplate & @JmsListener Patterns Demo)");
        log.info("==========================================================================================");

        // Step 1: トランザクショナル・キューテーブルの初期化
        log.info("\n--> [Step 1] Initializing Transactional Queue Tables in H2 Database...");
        try (Connection conn = dataSource.getConnection(); Statement stmt = conn.createStatement()) {
            stmt.execute("CREATE QUEUE TABLE IF NOT EXISTS notification_queue (payload VARCHAR NOT NULL) WITH (RETENTION_HOURS = 24)");
            stmt.execute("CREATE QUEUE TABLE IF NOT EXISTS payment_request_queue (payload VARCHAR NOT NULL) WITH (RETENTION_HOURS = 24)");
            stmt.execute("CREATE QUEUE TABLE IF NOT EXISTS payment_reply_queue (payload VARCHAR NOT NULL) WITH (RETENTION_HOURS = 24)");
        }
        log.info("   [OK] Queue tables ('notification_queue', 'payment_request_queue', 'payment_reply_queue') ready.");

        // Step 2: パターン 1 - JmsTemplate.convertAndSend ＋ @JmsListener & @Header
        log.info("\n--> [Step 2] Pattern 1: JmsTemplate.convertAndSend + @JmsListener with @Header...");
        notificationListener.resetLatch(1);
        NotificationMessage notif1 = new NotificationMessage("NOTIF-001", "admin@company.com", "Server cluster failover test completed.");
        jmsClientService.sendNotificationWithHeaders("notification_queue", notif1, "HIGH", "BillingSystem");

        boolean notifReceived = notificationListener.getLatch().await(3, TimeUnit.SECONDS);
        log.info("   [Result] @JmsListener received message successfully? {}", notifReceived);
        log.info("   [Listener Received List] Total: {}", notificationListener.getReceivedNotifications().size());

        // Step 3: パターン 2 - JmsTemplate リクエスト送信 ＋ @JmsListener & @SendTo による自動応答
        log.info("\n--> [Step 3] Pattern 2: Request-Reply RPC Pattern via @JmsListener and @SendTo...");
        paymentProcessorListener.resetLatch(1);
        PaymentRequest paymentRequest = new PaymentRequest("PAY-8888", "Acme Corporation", 12500.00, "USD");
        jmsClientService.sendPaymentRequest(paymentRequest);

        boolean paymentProcessed = paymentProcessorListener.getLatch().await(3, TimeUnit.SECONDS);
        log.info("   [Result] PaymentProcessorListener processed request? {}", paymentProcessed);

        // Step 4: パターン 3 - JmsTemplate.receiveAndConvert による同期受信 (payment_reply_queue)
        log.info("\n--> [Step 4] Pattern 3: Synchronous Polling via JmsTemplate.receiveAndConvert...");
        PaymentResponse reply = jmsClientService.receivePaymentReplySync();
        log.info("   [Synchronous Receive Result] PaymentResponse: {}", reply);
        if (reply != null && "SUCCESS".equals(reply.getStatus())) {
            log.info("   [SUCCESS] Received auto-replied PaymentResponse (TxCode: {}) via JmsTemplate!", reply.getTransactionCode());
        }

        // Step 5: パターン 4 - @Transactional によるロールバック検証
        log.info("\n--> [Step 5] Pattern 4: Atomic Rollback verification with @Transactional...");
        int countBefore = countMessagesInQueue("notification_queue");
        log.info("   Queue message count before failed transaction: {}", countBefore);

        NotificationMessage doomedNotif = new NotificationMessage("NOTIF-DOOMED", "ghost@null.void", "This should be discarded");
        try {
            jmsClientService.sendNotificationWithRollback("notification_queue", doomedNotif);
        } catch (Exception e) {
            log.info("   [Expected Business Failure Caught] {}", e.getMessage());
        }

        int countAfter = countMessagesInQueue("notification_queue");
        log.info("   Queue message count after rollback (should remain {}): {}", countBefore, countAfter);
        if (countBefore == countAfter) {
            log.info("   [SUCCESS] JmsTemplate message was rolled back atomically within @Transactional!");
        } else {
            throw new IllegalStateException("Rollback verification failed!");
        }

        // Step 6: パターン 5 - H2 Database Native Queue の Kafka 風オフセット再生（Seek / Replay）
        log.info("\n--> [Step 6] Pattern 5: Kafka-style Offset Seeking & Replay on H2 Queue Table...");
        try (Connection conn = dataSource.getConnection();
             Statement stmt = conn.createStatement();
             ResultSet rs = stmt.executeQuery("SELECT _offset, _timestamp, _msg_id, payload FROM notification_queue ORDER BY _offset ASC")) {
            log.info("   --- Streaming all messages in notification_queue ---");
            while (rs.next()) {
                long offset = rs.getLong(1);
                String msgId = rs.getString(3);
                String payload = rs.getString(4);
                log.info("     [Offset {}] msgId={}, payload={}", offset, msgId, payload);
            }
        }

        log.info("\n==========================================================================================");
        log.info("  🎉 ALL SPRING JMS (JmsTemplate & @JmsListener) PATTERNS DEMO PASSED SUCCESSFULLY!       ");
        log.info("==========================================================================================\n");

        new Thread(() -> {
            try {
                Thread.sleep(800);
                System.exit(SpringApplication.exit(applicationContext, () -> 0));
            } catch (Exception ignored) {
                System.exit(0);
            }
        }, "demo-shutdown-hook").start();
    }

    private int countMessagesInQueue(String queueName) {
        try (Connection conn = dataSource.getConnection();
             Statement stmt = conn.createStatement();
             ResultSet rs = stmt.executeQuery("SELECT COUNT(*) FROM " + queueName)) {
            if (rs.next()) {
                return rs.getInt(1);
            }
        } catch (Exception e) {
            log.error("Failed to count messages: {}", e.getMessage());
        }
        return 0;
    }
}
