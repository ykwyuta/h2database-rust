package com.example.jms.config;

import com.example.jms.provider.H2JmsConnectionFactory;
import jakarta.jms.ConnectionFactory;
import jakarta.jms.Session;
import org.springframework.context.annotation.Bean;
import org.springframework.context.annotation.Configuration;
import org.springframework.jms.annotation.EnableJms;
import org.springframework.jms.config.DefaultJmsListenerContainerFactory;
import org.springframework.jms.core.JmsTemplate;
import org.springframework.jms.support.converter.MappingJackson2MessageConverter;
import org.springframework.jms.support.converter.MessageConverter;
import org.springframework.jms.support.converter.MessageType;

import javax.sql.DataSource;

/**
 * Spring JMS (JmsTemplate, @JmsListener) の標準構成クラス。
 */
@Configuration
@EnableJms
public class JmsConfig {

    @Bean
    public ConnectionFactory connectionFactory(DataSource dataSource) {
        return new H2JmsConnectionFactory(dataSource);
    }

    /**
     * POJO と JSON 文字列 (TextMessage) の相互変換を行う MessageConverter。
     */
    @Bean
    public MessageConverter jacksonJmsMessageConverter() {
        return new JsonTextMessageConverter();
    }

    /**
     * 送信・同期受信を行う標準 JmsTemplate。
     */
    @Bean
    public JmsTemplate jmsTemplate(ConnectionFactory connectionFactory, MessageConverter messageConverter) {
        JmsTemplate template = new JmsTemplate(connectionFactory);
        template.setMessageConverter(messageConverter);
        template.setReceiveTimeout(3000);
        return template;
    }

    /**
     * @JmsListener アノテーション用リスナーコンテナファクトリ。
     */
    @Bean
    public DefaultJmsListenerContainerFactory jmsListenerContainerFactory(ConnectionFactory connectionFactory,
                                                                         MessageConverter messageConverter) {
        DefaultJmsListenerContainerFactory factory = new DefaultJmsListenerContainerFactory();
        factory.setConnectionFactory(connectionFactory);
        factory.setMessageConverter(messageConverter);
        factory.setConcurrency("1-1");
        factory.setSessionAcknowledgeMode(Session.AUTO_ACKNOWLEDGE);
        return factory;
    }
}
