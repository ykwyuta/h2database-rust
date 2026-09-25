package com.example.jms.provider;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import jakarta.jms.*;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import javax.sql.DataSource;
import java.sql.Connection;
import java.sql.PreparedStatement;
import java.sql.ResultSet;
import java.sql.SQLException;
import java.util.Iterator;
import java.util.Map;
import java.util.concurrent.atomic.AtomicBoolean;

public class H2JmsMessageConsumer implements MessageConsumer {
    private static final Logger log = LoggerFactory.getLogger(H2JmsMessageConsumer.class);
    private static final ObjectMapper OBJECT_MAPPER = new ObjectMapper();

    private final DataSource dataSource;
    private final Destination destination;
    private final String queueName;
    private long currentOffset = 0L;
    private final AtomicBoolean closed = new AtomicBoolean(false);
    private MessageListener messageListener;
    private Thread listenerThread;

    public H2JmsMessageConsumer(DataSource dataSource, Destination destination) throws JMSException {
        this.dataSource = dataSource;
        this.destination = destination;
        if (!(destination instanceof Queue)) {
            throw new InvalidDestinationException("Destination must be a Queue: " + destination);
        }
        this.queueName = ((Queue) destination).getQueueName();
    }

    public long getCurrentOffset() {
        return currentOffset;
    }

    public void seek(long offset) {
        this.currentOffset = offset;
        log.info("JMS Consumer: Seeked queue '{}' to offset {}", queueName, offset);
    }

    @Override
    public String getMessageSelector() throws JMSException {
        return null;
    }

    @Override
    public MessageListener getMessageListener() throws JMSException {
        return messageListener;
    }

    @Override
    public synchronized void setMessageListener(MessageListener listener) throws JMSException {
        this.messageListener = listener;
        if (listener != null && listenerThread == null) {
            listenerThread = new Thread(() -> {
                while (!closed.get()) {
                    try {
                        Message msg = receive(200);
                        if (msg != null && messageListener != null) {
                            messageListener.onMessage(msg);
                        }
                    } catch (Exception e) {
                        if (!closed.get()) {
                            log.debug("Polling loop message: {}", e.getMessage());
                        }
                    }
                }
            }, "h2-jms-consumer-" + queueName);
            listenerThread.setDaemon(true);
            listenerThread.start();
        }
    }

    @Override
    public Message receive() throws JMSException {
        return receive(0);
    }

    @Override
    public Message receive(long timeout) throws JMSException {
        if (closed.get()) {
            return null;
        }

        long deadline = timeout > 0 ? System.currentTimeMillis() + timeout : Long.MAX_VALUE;
        String sql = "SELECT _offset, payload FROM " + queueName + " WHERE _offset >= ? ORDER BY _offset ASC LIMIT 1";

        do {
            try (Connection conn = dataSource.getConnection();
                 PreparedStatement ps = conn.prepareStatement(sql)) {
                ps.setLong(1, currentOffset);
                try (ResultSet rs = ps.executeQuery()) {
                    if (rs.next()) {
                        long offset = rs.getLong(1);
                        String payload = rs.getString(2);

                        H2JmsTextMessage msg = new H2JmsTextMessage();
                        msg.setOffset(offset);
                        msg.setJMSDestination(destination);

                        boolean parsedEnvelope = false;
                        if (payload != null && payload.trim().startsWith("{")) {
                            try {
                                JsonNode node = OBJECT_MAPPER.readTree(payload);
                                if (node.has("__h2_envelope__") && node.get("__h2_envelope__").asBoolean()) {
                                    if (node.has("body") && !node.get("body").isNull()) {
                                        msg.setText(node.get("body").asText());
                                    }
                                    if (node.has("correlationId") && !node.get("correlationId").isNull()) {
                                        msg.setJMSCorrelationID(node.get("correlationId").asText());
                                    }
                                    if (node.has("replyTo") && !node.get("replyTo").isNull()) {
                                        msg.setJMSReplyTo(new H2JmsQueue(node.get("replyTo").asText()));
                                    }
                                    if (node.has("properties") && node.get("properties").isObject()) {
                                        Iterator<Map.Entry<String, JsonNode>> fields = node.get("properties").fields();
                                        while (fields.hasNext()) {
                                            Map.Entry<String, JsonNode> entry = fields.next();
                                            JsonNode v = entry.getValue();
                                            if (v.isBoolean()) {
                                                msg.setBooleanProperty(entry.getKey(), v.asBoolean());
                                            } else if (v.isInt()) {
                                                msg.setIntProperty(entry.getKey(), v.asInt());
                                            } else if (v.isLong()) {
                                                msg.setLongProperty(entry.getKey(), v.asLong());
                                            } else if (v.isDouble()) {
                                                msg.setDoubleProperty(entry.getKey(), v.asDouble());
                                            } else {
                                                msg.setStringProperty(entry.getKey(), v.asText());
                                            }
                                        }
                                    }
                                    parsedEnvelope = true;
                                }
                            } catch (Exception ignored) {
                            }
                        }
                        if (!parsedEnvelope) {
                            msg.setText(payload);
                        }

                        this.currentOffset = offset + 1;
                        return msg;
                    }
                }
            } catch (SQLException e) {
                if (closed.get()) return null;
                log.debug("Polling queue error (waiting for queue table): {}", e.getMessage());
            }

            if (timeout == 0) {
                break;
            }

            long remaining = deadline - System.currentTimeMillis();
            if (remaining <= 0) {
                break;
            }

            try {
                Thread.sleep(Math.min(remaining, 100));
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
                break;
            }
        } while (System.currentTimeMillis() < deadline && !closed.get());

        return null;
    }

    @Override
    public Message receiveNoWait() throws JMSException {
        return receive(0);
    }

    @Override
    public void close() throws JMSException {
        closed.set(true);
        if (listenerThread != null) {
            listenerThread.interrupt();
            listenerThread = null;
        }
    }
}
