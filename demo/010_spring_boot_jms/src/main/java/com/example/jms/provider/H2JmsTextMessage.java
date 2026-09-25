package com.example.jms.provider;

import jakarta.jms.Destination;
import jakarta.jms.JMSException;
import jakarta.jms.TextMessage;

import java.io.Serializable;
import java.util.*;

/**
 * H2 Database Rust 用 Jakarta JMS TextMessage 実装
 */
public class H2JmsTextMessage implements TextMessage, Serializable {
    private String text;
    private String messageId;
    private long timestamp;
    private String correlationId;
    private Destination replyTo;
    private Destination destination;
    private int deliveryMode = 1;
    private boolean redelivered = false;
    private String type;
    private long expiration = 0;
    private long deliveryTime = 0;
    private int priority = 4;
    private Long offset;

    private final Map<String, Object> properties = new HashMap<>();

    public H2JmsTextMessage() {
        this.timestamp = System.currentTimeMillis();
        this.messageId = "ID:" + UUID.randomUUID();
    }

    public H2JmsTextMessage(String text) {
        this();
        this.text = text;
    }

    public Long getOffset() {
        return offset;
    }

    public void setOffset(Long offset) {
        this.offset = offset;
    }

    @Override
    public void setText(String string) throws JMSException {
        this.text = string;
    }

    @Override
    public String getText() throws JMSException {
        return text;
    }

    @Override
    public String getJMSMessageID() throws JMSException {
        return messageId;
    }

    @Override
    public void setJMSMessageID(String id) throws JMSException {
        this.messageId = id;
    }

    @Override
    public long getJMSTimestamp() throws JMSException {
        return timestamp;
    }

    @Override
    public void setJMSTimestamp(long timestamp) throws JMSException {
        this.timestamp = timestamp;
    }

    @Override
    public byte[] getJMSCorrelationIDAsBytes() throws JMSException {
        return correlationId != null ? correlationId.getBytes() : null;
    }

    @Override
    public void setJMSCorrelationIDAsBytes(byte[] correlationID) throws JMSException {
        this.correlationId = correlationID != null ? new String(correlationID) : null;
    }

    @Override
    public void setJMSCorrelationID(String correlationID) throws JMSException {
        this.correlationId = correlationID;
    }

    @Override
    public String getJMSCorrelationID() throws JMSException {
        return correlationId;
    }

    @Override
    public Destination getJMSReplyTo() throws JMSException {
        return replyTo;
    }

    @Override
    public void setJMSReplyTo(Destination replyTo) throws JMSException {
        this.replyTo = replyTo;
    }

    @Override
    public Destination getJMSDestination() throws JMSException {
        return destination;
    }

    @Override
    public void setJMSDestination(Destination destination) throws JMSException {
        this.destination = destination;
    }

    @Override
    public int getJMSDeliveryMode() throws JMSException {
        return deliveryMode;
    }

    @Override
    public void setJMSDeliveryMode(int deliveryMode) throws JMSException {
        this.deliveryMode = deliveryMode;
    }

    @Override
    public boolean getJMSRedelivered() throws JMSException {
        return redelivered;
    }

    @Override
    public void setJMSRedelivered(boolean redelivered) throws JMSException {
        this.redelivered = redelivered;
    }

    @Override
    public String getJMSType() throws JMSException {
        return type;
    }

    @Override
    public void setJMSType(String type) throws JMSException {
        this.type = type;
    }

    @Override
    public long getJMSExpiration() throws JMSException {
        return expiration;
    }

    @Override
    public void setJMSExpiration(long expiration) throws JMSException {
        this.expiration = expiration;
    }

    @Override
    public long getJMSDeliveryTime() throws JMSException {
        return deliveryTime;
    }

    @Override
    public void setJMSDeliveryTime(long deliveryTime) throws JMSException {
        this.deliveryTime = deliveryTime;
    }

    @Override
    public int getJMSPriority() throws JMSException {
        return priority;
    }

    @Override
    public void setJMSPriority(int priority) throws JMSException {
        this.priority = priority;
    }

    @Override
    public void clearProperties() throws JMSException {
        properties.clear();
    }

    @Override
    public boolean propertyExists(String name) throws JMSException {
        return properties.containsKey(name);
    }

    @Override
    public boolean getBooleanProperty(String name) throws JMSException {
        Object val = properties.get(name);
        return val != null && Boolean.parseBoolean(val.toString());
    }

    @Override
    public byte getByteProperty(String name) throws JMSException {
        Object val = properties.get(name);
        return val != null ? Byte.parseByte(val.toString()) : 0;
    }

    @Override
    public short getShortProperty(String name) throws JMSException {
        Object val = properties.get(name);
        return val != null ? Short.parseShort(val.toString()) : 0;
    }

    @Override
    public int getIntProperty(String name) throws JMSException {
        Object val = properties.get(name);
        return val != null ? Integer.parseInt(val.toString()) : 0;
    }

    @Override
    public long getLongProperty(String name) throws JMSException {
        Object val = properties.get(name);
        return val != null ? Long.parseLong(val.toString()) : 0L;
    }

    @Override
    public float getFloatProperty(String name) throws JMSException {
        Object val = properties.get(name);
        return val != null ? Float.parseFloat(val.toString()) : 0.0f;
    }

    @Override
    public double getDoubleProperty(String name) throws JMSException {
        Object val = properties.get(name);
        return val != null ? Double.parseDouble(val.toString()) : 0.0;
    }

    @Override
    public String getStringProperty(String name) throws JMSException {
        Object val = properties.get(name);
        return val != null ? val.toString() : null;
    }

    @Override
    public Object getObjectProperty(String name) throws JMSException {
        return properties.get(name);
    }

    @Override
    public Enumeration<?> getPropertyNames() throws JMSException {
        return Collections.enumeration(properties.keySet());
    }

    @Override
    public void setBooleanProperty(String name, boolean value) throws JMSException {
        properties.put(name, value);
    }

    @Override
    public void setByteProperty(String name, byte value) throws JMSException {
        properties.put(name, value);
    }

    @Override
    public void setShortProperty(String name, short value) throws JMSException {
        properties.put(name, value);
    }

    @Override
    public void setIntProperty(String name, int value) throws JMSException {
        properties.put(name, value);
    }

    @Override
    public void setLongProperty(String name, long value) throws JMSException {
        properties.put(name, value);
    }

    @Override
    public void setFloatProperty(String name, float value) throws JMSException {
        properties.put(name, value);
    }

    @Override
    public void setDoubleProperty(String name, double value) throws JMSException {
        properties.put(name, value);
    }

    @Override
    public void setStringProperty(String name, String value) throws JMSException {
        properties.put(name, value);
    }

    @Override
    public void setObjectProperty(String name, Object value) throws JMSException {
        properties.put(name, value);
    }

    @Override
    public void acknowledge() throws JMSException {
    }

    @Override
    public void clearBody() throws JMSException {
        this.text = null;
    }

    @Override
    @SuppressWarnings("unchecked")
    public <T> T getBody(Class<T> c) throws JMSException {
        if (c.isInstance(text)) {
            return (T) text;
        }
        throw new JMSException("Cannot assign body to " + c.getName());
    }

    @Override
    public boolean isBodyAssignableTo(Class c) throws JMSException {
        return c.isAssignableFrom(String.class);
    }

    @Override
    public String toString() {
        return "H2JmsTextMessage{" +
                "offset=" + offset +
                ", messageId='" + messageId + '\'' +
                ", text='" + text + '\'' +
                '}';
    }
}
