package com.example.jms.mapper;

import com.example.jms.model.QueueMessage;
import org.apache.ibatis.annotations.Param;

import java.util.List;

public interface QueueMapper {

    void dropOrderEventsQueue();

    void createOrderEventsQueue();

    int enqueueMessage(@Param("payload") String payload);

    List<QueueMessage> fetchMessagesFromOffset(@Param("fromOffset") Long fromOffset);
}
