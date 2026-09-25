package com.example.jms.service;

import com.example.jms.mapper.OrderMapper;
import com.example.jms.model.Order;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.springframework.jms.core.JmsTemplate;
import org.springframework.stereotype.Service;
import org.springframework.transaction.annotation.Transactional;

/**
 * 注文処理ビジネスサービス。
 * Spring の同一トランザクション (@Transactional) 内で、
 * MyBatis によるテーブル更新と JmsTemplate によるメッセージ送信をアトミックに実行します。
 */
@Service
public class OrderProcessingService {
    private static final Logger log = LoggerFactory.getLogger(OrderProcessingService.class);

    private final OrderMapper orderMapper;
    private final JmsTemplate jmsTemplate;

    public OrderProcessingService(OrderMapper orderMapper, JmsTemplate jmsTemplate) {
        this.orderMapper = orderMapper;
        this.jmsTemplate = jmsTemplate;
    }

    /**
     * 正常シナリオ:
     * 注文レコード挿入と JMS メッセージ送信が同一トランザクションでコミットされます。
     */
    @Transactional
    public void createOrderSuccess(Order order) {
        log.info("Creating order in DB: id={}, customer={}", order.getId(), order.getCustomerName());
        orderMapper.insertOrder(order);

        String eventPayload = String.format(
                "{\"eventType\":\"ORDER_CREATED\",\"orderId\":%d,\"customer\":\"%s\",\"amount\":%.2f}",
                order.getId(), order.getCustomerName(), order.getAmount()
        );

        log.info("Sending order event via standard JmsTemplate: {}", eventPayload);
        jmsTemplate.convertAndSend("order_events_queue", eventPayload);
        log.info("Order and JMS message prepared in transaction. Committing...");
    }

    /**
     * ロールバックシナリオ:
     * 途中で例外が発生した場合、注文レコードと送信準備されたJMSメッセージの双方が
     * 同一トランザクションとして完全にロールバックされます（Transactional Outbox パターン不要）。
     */
    @Transactional
    public void createOrderWithRollback(Order order) {
        log.info("Creating order in DB (will be rolled back): id={}", order.getId());
        orderMapper.insertOrder(order);

        String eventPayload = String.format(
                "{\"eventType\":\"ORDER_CANCELLED\",\"orderId\":%d,\"customer\":\"%s\"}",
                order.getId(), order.getCustomerName()
        );

        log.info("Sending order event via standard JmsTemplate: {}", eventPayload);
        jmsTemplate.convertAndSend("order_events_queue", eventPayload);

        log.warn("Simulating unexpected runtime failure in business logic...");
        throw new IllegalStateException("Simulated business logic failure! Rollback triggered.");
    }
}
