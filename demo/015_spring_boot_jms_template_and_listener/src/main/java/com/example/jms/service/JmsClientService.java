package com.example.jms.service;

import com.example.jms.model.NotificationMessage;
import com.example.jms.model.PaymentRequest;
import com.example.jms.model.PaymentResponse;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.springframework.jms.core.JmsTemplate;
import org.springframework.stereotype.Service;
import org.springframework.transaction.annotation.Transactional;

/**
 * JmsTemplate を利用したクライアントサービス。
 * convertAndSend, send (MessagePostProcessor によるヘッダー付与),
 * receiveAndConvert (同期受信), @Transactional によるロールバックを実演します。
 */
@Service
public class JmsClientService {
    private static final Logger log = LoggerFactory.getLogger(JmsClientService.class);

    private final JmsTemplate jmsTemplate;

    public JmsClientService(JmsTemplate jmsTemplate) {
        this.jmsTemplate = jmsTemplate;
    }

    /**
     * パターン 1: MessagePostProcessor によるヘッダー付与と convertAndSend
     */
    public void sendNotificationWithHeaders(String queueName, NotificationMessage notification, String priority, String sourceApp) {
        log.info("Sending Notification via JmsTemplate to '{}': {}", queueName, notification);
        jmsTemplate.convertAndSend(queueName, notification, message -> {
            message.setStringProperty("priorityLevel", priority);
            message.setStringProperty("sourceApp", sourceApp);
            return message;
        });
    }

    /**
     * パターン 2: POJO の直接送信 (Request-Reply 用のリクエスト送信)
     */
    public void sendPaymentRequest(PaymentRequest request) {
        log.info("Sending PaymentRequest via JmsTemplate to 'payment_request_queue': {}", request);
        jmsTemplate.convertAndSend("payment_request_queue", request);
    }

    /**
     * パターン 3: 同期受信 (receiveAndConvert)
     */
    public PaymentResponse receivePaymentReplySync() {
        log.info("Performing synchronous receiveAndConvert from 'payment_reply_queue' (timeout 3000ms)...");
        Object obj = jmsTemplate.receiveAndConvert("payment_reply_queue");
        if (obj instanceof PaymentResponse) {
            return (PaymentResponse) obj;
        }
        log.warn("Received response is null or not PaymentResponse: {}", obj);
        return null;
    }

    /**
     * パターン 4: @Transactional 下でのキュー送信とロールバック
     */
    @Transactional
    public void sendNotificationWithRollback(String queueName, NotificationMessage notification) {
        log.info("Sending Notification inside @Transactional (will rollback): {}", notification);
        jmsTemplate.convertAndSend(queueName, notification);

        log.warn("Simulating unexpected runtime failure to trigger rollback...");
        throw new RuntimeException("Simulated business exception for JMS rollback verification");
    }
}
