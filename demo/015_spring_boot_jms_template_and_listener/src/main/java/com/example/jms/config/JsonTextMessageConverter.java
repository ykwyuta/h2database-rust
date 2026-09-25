package com.example.jms.config;

import com.fasterxml.jackson.databind.ObjectMapper;
import jakarta.jms.JMSException;
import jakarta.jms.Message;
import jakarta.jms.Session;
import jakarta.jms.TextMessage;
import org.springframework.jms.support.converter.MessageConversionException;
import org.springframework.jms.support.converter.MessageConverter;

/**
 * POJO と JSON 文字列 (TextMessage) を相互変換する堅牢なカスタム MessageConverter。
 * _type プロパティを用いて POJO クラスを特定し、自動マッピングします。
 */
public class JsonTextMessageConverter implements MessageConverter {
    private final ObjectMapper objectMapper = new ObjectMapper();

    @Override
    public Message toMessage(Object object, Session session) throws JMSException, MessageConversionException {
        try {
            String json = objectMapper.writeValueAsString(object);
            TextMessage message = session.createTextMessage(json);
            message.setStringProperty("_type", object.getClass().getName());
            return message;
        } catch (Exception e) {
            throw new MessageConversionException("Failed to convert object to JSON TextMessage: " + e.getMessage(), e);
        }
    }

    @Override
    public Object fromMessage(Message message) throws JMSException, MessageConversionException {
        if (!(message instanceof TextMessage)) {
            throw new MessageConversionException("Expected TextMessage but received: " + message.getClass().getName());
        }
        TextMessage textMessage = (TextMessage) message;
        try {
            String json = textMessage.getText();
            if (json == null) {
                return null;
            }
            String typeName = textMessage.getStringProperty("_type");
            if (typeName != null) {
                Class<?> clazz = Class.forName(typeName);
                return objectMapper.readValue(json, clazz);
            }
            return json;
        } catch (Exception e) {
            throw new MessageConversionException("Failed to convert JSON TextMessage to POJO: " + e.getMessage(), e);
        }
    }
}
