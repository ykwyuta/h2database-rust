package com.example.jms.provider;

import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.node.ObjectNode;
import jakarta.jms.*;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.springframework.jdbc.datasource.DataSourceUtils;

import javax.sql.DataSource;
import java.sql.Connection;
import java.sql.PreparedStatement;
import java.sql.SQLException;
import java.util.Enumeration;

public class H2JmsMessageProducer implements MessageProducer {
    private static final Logger log = LoggerFactory.getLogger(H2JmsMessageProducer.class);

    private final DataSource dataSource;
    private Destination defaultDestination;
    private int deliveryMode = DeliveryMode.PERSISTENT;
    private int priority = 4;
    private long timeToLive = 0;
    private long deliveryDelay = 0;
    private boolean disableMessageID = false;
    private boolean disableMessageTimestamp = false;
    private boolean closed = false;

    public H2JmsMessageProducer(DataSource dataSource, Destination defaultDestination) {
        this.dataSource = dataSource;
        this.defaultDestination = defaultDestination;
    }

    @Override
    public void setDisableMessageID(boolean value) throws JMSException {
        this.disableMessageID = value;
    }

    @Override
    public boolean getDisableMessageID() throws JMSException {
        return disableMessageID;
    }

    @Override
    public void setDisableMessageTimestamp(boolean value) throws JMSException {
        this.disableMessageTimestamp = value;
    }

    @Override
    public boolean getDisableMessageTimestamp() throws JMSException {
        return disableMessageTimestamp;
    }

    @Override
    public void setDeliveryMode(int deliveryMode) throws JMSException {
        this.deliveryMode = deliveryMode;
    }

    @Override
    public int getDeliveryMode() throws JMSException {
        return deliveryMode;
    }

    @Override
    public void setPriority(int defaultPriority) throws JMSException {
        this.priority = defaultPriority;
    }

    @Override
    public int getPriority() throws JMSException {
        return priority;
    }

    @Override
    public void setTimeToLive(long timeToLive) throws JMSException {
        this.timeToLive = timeToLive;
    }

    @Override
    public long getTimeToLive() throws JMSException {
        return timeToLive;
    }

    @Override
    public void setDeliveryDelay(long deliveryDelay) throws JMSException {
        this.deliveryDelay = deliveryDelay;
    }

    @Override
    public long getDeliveryDelay() throws JMSException {
        return deliveryDelay;
    }

    @Override
    public Destination getDestination() throws JMSException {
        return defaultDestination;
    }

    @Override
    public void close() throws JMSException {
        this.closed = true;
    }

    @Override
    public void send(Message message) throws JMSException {
        send(defaultDestination, message);
    }

    @Override
    public void send(Message message, int deliveryMode, int priority, long timeToLive) throws JMSException {
        send(defaultDestination, message, deliveryMode, priority, timeToLive);
    }

    @Override
    public void send(Destination destination, Message message) throws JMSException {
        send(destination, message, this.deliveryMode, this.priority, this.timeToLive);
    }

    @Override
    public void send(Destination destination, Message message, int deliveryMode, int priority, long timeToLive) throws JMSException {
        if (closed) {
            throw new jakarta.jms.IllegalStateException("MessageProducer is closed");
        }
        if (destination == null) {
            throw new InvalidDestinationException("Destination cannot be null");
        }
        if (!(destination instanceof Queue)) {
            throw new InvalidDestinationException("Only Queue destination is supported in this demo: " + destination);
        }

        Queue queue = (Queue) destination;
        String queueName = queue.getQueueName();

        String bodyText = "";
        if (message instanceof TextMessage) {
            bodyText = ((TextMessage) message).getText();
        } else if (message != null) {
            bodyText = message.toString();
        }

        // JMS ヘッダーおよびプロパティをエンベロープとして JSON にパッケージング
        ObjectMapper mapper = new ObjectMapper();
        ObjectNode envelope = mapper.createObjectNode();
        envelope.put("__h2_envelope__", true);
        envelope.put("body", bodyText != null ? bodyText : "");

        if (message.getJMSCorrelationID() != null) {
            envelope.put("correlationId", message.getJMSCorrelationID());
        }
        if (message.getJMSReplyTo() instanceof Queue) {
            envelope.put("replyTo", ((Queue) message.getJMSReplyTo()).getQueueName());
        }

        ObjectNode propsNode = mapper.createObjectNode();
        Enumeration<?> propNames = message.getPropertyNames();
        if (propNames != null) {
            while (propNames.hasMoreElements()) {
                String name = (String) propNames.nextElement();
                Object val = message.getObjectProperty(name);
                if (val instanceof Boolean) {
                    propsNode.put(name, (Boolean) val);
                } else if (val instanceof Integer) {
                    propsNode.put(name, (Integer) val);
                } else if (val instanceof Long) {
                    propsNode.put(name, (Long) val);
                } else if (val instanceof Double) {
                    propsNode.put(name, (Double) val);
                } else if (val != null) {
                    propsNode.put(name, val.toString());
                }
            }
        }
        envelope.set("properties", propsNode);

        String payload = envelope.toString();

        // Spring の DataSourceUtils から現在のトランザクション内コネクションを取得
        Connection conn = DataSourceUtils.getConnection(dataSource);
        String sql = "INSERT INTO " + queueName + " (payload) VALUES (?)";
        try (PreparedStatement ps = conn.prepareStatement(sql)) {
            ps.setString(1, payload);
            ps.executeUpdate();
            log.info("JMS Producer: Successfully enqueued message to '{}': body='{}'", queueName, bodyText);
        } catch (SQLException e) {
            throw new JMSException("Failed to enqueue message into " + queueName + ": " + e.getMessage());
        } finally {
            DataSourceUtils.releaseConnection(conn, dataSource);
        }
    }

    @Override
    public void send(Message message, CompletionListener completionListener) throws JMSException {
        send(defaultDestination, message);
        if (completionListener != null) {
            completionListener.onCompletion(message);
        }
    }

    @Override
    public void send(Message message, int deliveryMode, int priority, long timeToLive, CompletionListener completionListener) throws JMSException {
        send(defaultDestination, message, deliveryMode, priority, timeToLive);
        if (completionListener != null) {
            completionListener.onCompletion(message);
        }
    }

    @Override
    public void send(Destination destination, Message message, CompletionListener completionListener) throws JMSException {
        send(destination, message);
        if (completionListener != null) {
            completionListener.onCompletion(message);
        }
    }

    @Override
    public void send(Destination destination, Message message, int deliveryMode, int priority, long timeToLive, CompletionListener completionListener) throws JMSException {
        send(destination, message, deliveryMode, priority, timeToLive);
        if (completionListener != null) {
            completionListener.onCompletion(message);
        }
    }
}
