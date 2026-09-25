package com.example.jms.provider;

import jakarta.jms.*;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import javax.sql.DataSource;
import java.sql.Connection;
import java.sql.PreparedStatement;
import java.sql.ResultSet;
import java.sql.SQLException;
import java.util.concurrent.atomic.AtomicBoolean;

/**
 * H2 Database Rust 用 MessageConsumer 実装。
 * キューテーブルに対して内部オフセット (_offset) を用いて順次走査を行い、
 * Kafka 風のシーク・再生が可能です。
 */
public class H2JmsMessageConsumer implements MessageConsumer {
    private static final Logger log = LoggerFactory.getLogger(H2JmsMessageConsumer.class);

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

    /**
     * Kafka 風に読み取り開始オフセットをシーク（巻き戻し・ジャンプ）します。
     */
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
                            log.error("Error in message listener loop: {}", e.getMessage());
                        }
                    }
                }
            }, "h2-jms-listener-" + queueName);
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

                        H2JmsTextMessage msg = new H2JmsTextMessage(payload);
                        msg.setOffset(offset);
                        msg.setJMSDestination(destination);

                        // 次回読み取りは offset + 1 から
                        this.currentOffset = offset + 1;
                        return msg;
                    }
                }
            } catch (SQLException e) {
                // テーブルがまだ作られていない場合などのハンドリング
                if (closed.get()) return null;
                log.debug("Polling queue error (table might not exist yet): {}", e.getMessage());
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
